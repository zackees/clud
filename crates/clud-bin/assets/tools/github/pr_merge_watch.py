#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""pr_merge_watch.py — fail-fast PR-check waiter for clud.

Polls a GitHub PR's checks; returns the moment a required check fails,
new review activity arrives, the PR closes/merges, or the timeout fires.
On non-success exits (and defensively on success), cancels still-running
workflow runs on the PR's head SHA so we stop burning matrix minutes on
results we've already decided to ignore.

Invoked via `"$CLUD_EXE" tool run github/pr_merge_watch.py …` so UV_CACHE_DIR
is pinned to ~/.clud/cache/uv per the three-layer enforcement (see
issue #408).

Immediately announces a durable repo-local JSONL log under
`.clud/logs/pr-merge-watch/`. The first record is a UTC `START`; later
records use monotonic seconds relative to that start.

Exit codes:
  0  all required checks green AND mergeable=MERGEABLE
  1  at least one required check failed (details on stdout)
  2  new review activity (unresolved coderabbit/human review)
  3  PR closed or merged out from under us
  4  timeout (configurable via --timeout, default 60min)
  5  approval required: a workflow run or check is `action_required` (a fork
     PR waiting for a maintainer); reported immediately, never pending
  6  never reported: a required check has no check run while every workflow
     run on the head commit has finished (path/branch filters, a
     `pull_request_target` run on the base commit, ...)
  7  stale: GitHub marked a required check `stale` (14 days incomplete); it
     never resolves on its own, a re-run is needed
  8  NO_CHECKS: no check will ever report (#1418): the base branch has no
     workflows, the head commit carries a skip marker (`[skip ci]`, ...), or
     the rollup stayed empty for the grace period (`--no-checks-grace`,
     default 60 s). The final event names the reason and `mergeStateStatus`;
     a `CLEAN` PR is mergeable as is
  9  CONFLICT: `mergeable=CONFLICTING` / `mergeStateStatus=DIRTY`, or
     `mergeable` stayed `UNKNOWN` for too many polls while checks were green
 10  GITHUB_UNREACHABLE: gh failed (auth, rate limit, network) for several
     polls in a row, or no repository could be resolved; the final event
     carries gh's stderr
 11  QUEUED: a run waited longer than `--max-queued` to start (off by default)
 130/143  killed by SIGINT/SIGTERM: a final `EXIT` event with reason `killed`,
     and nothing is cancelled

A watch only ever cancels runs on the head SHA it started on (#1418): when the
head moves it logs `head_moved`, keeps judging the new head, and skips any
cancellation. `--timeout` defaults to `$CLUD_PR_MERGE_WATCH_TIMEOUT` (else
3600 s); a caller whose tool call is capped must pass a lower value, or the
watch outlives it.

Supersession rule (#1330). Check runs are judged on the PR's *current* head
commit only, grouped by (workflow file, check name) -- never the display
`name:` -- and ordered by check-run id, not run id:
  - a `cancelled` check is replaced by any newer check in its group, even one
    still queued or in progress;
  - a completed result is replaced only by a newer check that actually ran
    (anything but `skipped`), so a skip never hides an older real failure;
  - a cancelled check with no replacement fails only when no newer run of
    its workflow exists on the head commit in any state (queued included);
  - `success`, `neutral` and `skipped` pass, a required `skipped` included;
  - a cancellation-derived failure is acted on only after re-reading the
    PR's head: if the head moved, the verdict is dropped and the watch
    continues on the new commit;
  - on a failure in workflow X only X's runs at or below the failing run are
    cancelled; newer runs and other workflows are left alone.
`merge_group` runs and check runs for any other commit are ignored; legacy
commit statuses keep GitHub's newest-per-context result.

The exit code IS the result — do not pipe this through `tail`, `grep` or
`head`. A pipeline reports the *last* stage's status, so every one of the
codes above collapses to whatever the filter exited with, and `tail` buffers
to EOF so the per-poll progress lines vanish too. Redirect to a file and read
it, or run it bare.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import sys
import time
from dataclasses import dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from typing import TextIO

from running_process import PIPE, RunningProcess, TimeoutExpired

EXIT_GREEN = 0
EXIT_REQUIRED_FAIL = 1
EXIT_REVIEW_ACTIVITY = 2
EXIT_PR_CLOSED = 3
EXIT_TIMEOUT = 4
EXIT_APPROVAL_REQUIRED = 5
EXIT_NEVER_REPORTED = 6
EXIT_STALE = 7
EXIT_NO_CHECKS = 8
EXIT_CONFLICT = 9
EXIT_GITHUB_UNREACHABLE = 10
EXIT_QUEUED = 11

DEFAULT_TIMEOUT_SEC = 3600
DEFAULT_NO_CHECKS_GRACE_SEC = 60
# Every gh call is bounded (#1418): a stalled connection must not block the
# loop past --timeout, which is only checked between polls.
GH_CALL_TIMEOUT_SEC = 30.0
MAX_CONSECUTIVE_API_FAILURES = 3
MERGEABLE_UNKNOWN_MAX_POLLS = 6
# An immediate no-checks reason still waits this long: `pull_request_target`
# workflows and check apps can register seconds after a `[skip ci]` push.
NO_CHECKS_SETTLE_SEC = 5
SKIP_CI_MARKER = re.compile(r"\[(?:skip ci|ci skip|no ci|skip actions|actions skip)\]", re.I)
QUEUED_STATUSES = {"queued", "waiting", "pending", "requested"}

CANCEL_ON_CHOICES = {"fail", "review", "timeout", "closed", "always", "never"}
CANCEL_ON_DEFAULTS = {"fail", "review", "timeout", "closed"}
CANCEL_MODE_CHOICES = {"runs", "jobs", "none"}


def _utc_now() -> datetime:
    return datetime.now(UTC)


def _utc_text(value: datetime) -> str:
    return value.astimezone(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _watch_root() -> Path:
    result = RunningProcess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0 and result.stdout.strip():
        return Path(result.stdout.strip())
    return Path.cwd()


@dataclass
class WatchLog:
    """Immediately announced, repo-local JSONL event log."""

    path: Path
    display_path: str
    started_monotonic: float
    stream: TextIO
    closed: bool = False

    @classmethod
    def create(cls, pr: int, repo: str | None, *, root: Path | None = None) -> WatchLog:
        started_at = _utc_now()
        started_monotonic = time.monotonic()
        millis = started_at.microsecond // 1000
        filename = f"{started_at:%Y%m%dT%H%M%S}.{millis:03d}Z-pr-{pr}.jsonl"
        display_path = f".clud/logs/pr-merge-watch/{filename}"
        base_root = root or _watch_root()
        path = base_root / Path(display_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        suffix = 2
        while path.exists():
            filename = f"{started_at:%Y%m%dT%H%M%S}.{millis:03d}Z-pr-{pr}-{suffix}.jsonl"
            display_path = f".clud/logs/pr-merge-watch/{filename}"
            path = base_root / Path(display_path)
            suffix += 1
        stream = path.open("w", encoding="utf-8", newline="\n")
        log = cls(path, display_path, started_monotonic, stream)
        log._write(
            {
                "v": 1,
                "ts": _utc_text(started_at),
                "elapsed_sec": 0.0,
                "event": "START",
                "repo": repo or "origin",
                "pr": pr,
                "log_path": display_path,
            }
        )
        print(f"LOG {display_path}", file=sys.stderr, flush=True)
        return log

    def _write(self, record: dict) -> None:
        self.stream.write(json.dumps(record, separators=(",", ":")) + "\n")
        self.stream.flush()

    def emit(self, event: str, **fields: object) -> None:
        if self.closed:
            return
        record = {
            "v": 1,
            "elapsed_sec": round(max(0.0, time.monotonic() - self.started_monotonic), 2),
            "event": event,
        }
        record.update(fields)
        self._write(record)

    def close(self) -> None:
        if self.closed:
            return
        self.stream.close()
        self.closed = True


# Every line of a GitHub Actions log carries a prefix the anchored patterns
# below must not see: the REST job-log endpoint emits
# `2026-09-03T19:45:30.9660131Z <line>`, and `gh run view --log-failed` emits
# `<job>\t<step>\t2026-…Z <line>`. Without stripping it, `^FAILED`,
# `^error` and `^thread .*? panicked` can never match a real log — which is
# how a pytest failure fell through to the transient-network pattern and was
# reported to the caller as "network/transient".
LOG_LINE_PREFIX = re.compile(
    r"^(?:[^\t]*\t[^\t]*\t)?\d{4}-\d{2}-\d{2}T[\d:.]+Z\s?"
)

# GitHub's workflow-command annotations. `##[error]` is how a step reports the
# thing that actually failed it -- `##[error]failing Rust harnesses:
# reaper-<hash>.exe (rc=-1073740791)` was the entire explanation for one red,
# and the anchored patterns below could not see it through the prefix.
GHA_ANNOTATION = re.compile(r"^##\[(?:error|warning)\]")

# Cargo's per-test result lines. These are NOT errors, and matching them is
# not hypothetical: `test tests::oversized_codes_are_truncated_not_panicked_on
# ... ok` contains the substring "panicked", so an unanchored search reported
# a PASSING test as the first error of a failed run. A first-error line that
# names the wrong thing is worse than none -- it sends the reader somewhere
# real and irrelevant.
CARGO_TEST_OUTCOME = re.compile(r"^test \S+ \.\.\. (ok|ignored)\b")

# A guard on pathological logs only. Real failure logs run to a few thousand
# lines; the summary that names the failing test is at the *end*, so this is
# deliberately not a head slice.
MAX_LOG_LINES = 100_000


def normalize_log(text: str) -> str:
    """Strip the runner's per-line prefix so anchored patterns mean what they say."""
    lines = text.splitlines()[:MAX_LOG_LINES]
    return "\n".join(LOG_LINE_PREFIX.sub("", line) for line in lines)


# First-error classifier patterns, applied to the normalized log.
# Order matters: first match wins.
CLASSIFIERS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"^Diff in .*?:\d+:|run `cargo fmt", re.MULTILINE), "rustfmt drift"),
    (re.compile(r"^error: .*?clippy::|warning:.*?clippy::", re.MULTILINE), "clippy warning"),
    (re.compile(r"^error\[E\d+\]:|^error: could not compile", re.MULTILINE), "compile error"),
    (
        re.compile(
            r"^thread .*? panicked at"
            r"|FAILED \(\d+\)"
            r"|test result: FAILED"
            # A harness that died rather than reported: GitHub's step
            # annotation is all that survives when the process aborts.
            r"|^##\[error\]failing Rust harnesses:"
            # pytest: the summary line and the inline per-test marker. Without
            # these a timed-out pytest case matched "timed out" below and was
            # mislabelled transient.
            r"|^FAILED \S+::"
            r"|short test summary info"
            r"|^E\s+\w*(?:Error|Exception|AssertionError)",
            re.MULTILINE,
        ),
        "test failure",
    ),
    (
        re.compile(r"^ruff (check|format).*?(failed|error)", re.MULTILINE | re.IGNORECASE),
        "ruff violation",
    ),
    (
        re.compile(
            r"timed out|connection refused|temporary failure|EAI_AGAIN|503 Service",
            re.MULTILINE | re.IGNORECASE,
        ),
        "network/transient",
    ),
]


@dataclass
class GhResult:
    """Outcome of one gh CLI call."""

    exit_code: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.exit_code == 0


LAST_GH_ERROR = ""


def gh_error_note(text: str) -> None:
    """Remember the latest gh failure, for the final unreachable report."""
    global LAST_GH_ERROR
    if text.strip():
        LAST_GH_ERROR = text.strip()[-2000:]


def gh(*args: str, check: bool = False, timeout: float | None = None) -> GhResult:
    """Run gh with the supplied args; return the captured outcome.

    `timeout` bounds the call (default `GH_CALL_TIMEOUT_SEC`): a call that
    would otherwise hang the loop is abandoned and reported as a failed call.
    """
    if timeout is None:
        timeout = GH_CALL_TIMEOUT_SEC
    try:
        # #1175: running-process merges stderr into stdout unless it is asked
        # for separately, and `capture_output=True` alone is not asking.
        # Without `stderr=PIPE` every `GhResult.stderr` was `None`, so the
        # first failed cancel crashed `_report_cancel` on `"HTTP 403" in None`.
        res = RunningProcess.run(
            ["gh", *args], capture_output=True, stderr=PIPE, text=True, timeout=timeout
        )
    except (TimeoutError, TimeoutExpired):
        # running-process raises its own `TimeoutExpired` (a
        # `subprocess.TimeoutExpired`, not a `TimeoutError`); catching only
        # the builtin let a hung probe escape as a crash (#1175 review).
        message = f"gh {' '.join(args)} timed out after {timeout}s"
        gh_error_note(message)
        return GhResult(124, "", message)
    stdout = res.stdout or ""
    stderr = res.stderr or ""
    if res.returncode != 0:
        gh_error_note(stderr or f"gh {' '.join(args)} exited {res.returncode}")
    if check and res.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {stderr.strip()}")
    return GhResult(res.returncode, stdout, stderr)


def gh_json(*args: str) -> object | None:
    """Run gh and parse stdout as JSON; return None on any failure."""
    r = gh(*args)
    if not r.ok or not r.stdout.strip():
        return None
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return None


@dataclass
class PRSnapshot:
    """One sample of the PR's gateable state."""

    number: int
    state: str  # OPEN | MERGED | CLOSED
    mergeable: str  # MERGEABLE | CONFLICTING | UNKNOWN
    head_sha: str
    base_ref: str
    head_ref: str = ""
    merge_state: str = "UNKNOWN"  # mergeStateStatus: CLEAN | DIRTY | BEHIND | BLOCKED | ...

    @classmethod
    def fetch(cls, pr: int, repo: str | None) -> PRSnapshot | None:
        args = ["pr", "view", str(pr)]
        if repo:
            args += ["--repo", repo]
        args += [
            "--json",
            "number,state,mergeable,headRefOid,baseRefName,headRefName,mergeStateStatus",
        ]
        data = gh_json(*args)
        if not isinstance(data, dict):
            return None
        return cls(
            number=int(data.get("number", pr)),
            state=str(data.get("state", "UNKNOWN")),
            mergeable=str(data.get("mergeable", "UNKNOWN")),
            head_sha=str(data.get("headRefOid", "")),
            base_ref=str(data.get("baseRefName", "main")),
            head_ref=str(data.get("headRefName") or ""),
            merge_state=str(data.get("mergeStateStatus") or "UNKNOWN"),
        )


@dataclass
class CheckRow:
    name: str
    bucket: str  # pass | fail | pending | skipping
    state: str
    link: str | None = None
    job_id: str | None = None  # populated lazily for failing checks


def check_counts(checks: list[CheckRow]) -> dict[str, int]:
    counts = {"total": len(checks), "pending": 0, "failed": 0, "succeeded": 0, "skipped": 0}
    for check in checks:
        bucket = check.bucket.lower()
        if bucket == "pass":
            counts["succeeded"] += 1
        elif bucket in {"fail", "cancel"}:
            counts["failed"] += 1
        elif bucket in {"skipping", "skip"}:
            counts["skipped"] += 1
        else:
            counts["pending"] += 1
    return counts


# ---------- head-commit check judgment (#1330) --------------------------------

PASSING_CONCLUSIONS = {"success", "neutral", "skipped"}
# A full-mode PR with re-runs passes 100 check runs; a runaway guard only.
MAX_PAGES = 50
PER_PAGE = 100


@dataclass(frozen=True)
class HeadChecks:
    """Raw REST data for one head commit, as GitHub returned it."""

    check_runs: list[dict]
    workflow_runs: list[dict]
    statuses: list[dict] = field(default_factory=list)


@dataclass
class CheckJudgment:
    """The judged state of one (workflow file, check name) group."""

    name: str
    workflow: str
    state: str  # pass | fail | pending | approval_required | stale | superseded
    conclusion: str
    check_run_id: int | None
    run_id: int | None
    link: str | None
    required: bool
    workflow_broken: bool = False  # startup_failure: the workflow, not the code
    cancelled: bool = False  # the failure is derived from a cancellation

    def as_check_row(self) -> CheckRow:
        bucket = {"pass": "pass", "superseded": "pass", "pending": "pending"}.get(
            self.state, "fail"
        )
        return CheckRow(self.name, bucket, self.conclusion.upper(), self.link)


@dataclass
class Verdict:
    """What the head commit's checks say, after the supersession rule."""

    state: str  # pending | pass | fail | approval_required | never_reported | stale
    judgments: list[CheckJudgment]
    failing: list[CheckJudgment]
    advisory_failing: list[CheckJudgment]
    # workflow key -> highest failing run id: the cancellation scope.
    failing_run_ids: dict[str, int]
    missing: list[str]
    notes: list[str]

    @property
    def cancellation_derived(self) -> bool:
        return any(j.cancelled for j in self.failing)


def _as_int(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _lower(value: object) -> str:
    return str(value or "").lower()


def _workflow_key(run: dict) -> str:
    """The workflow's file path (or id), never its display `name:`."""
    path = run.get("path")
    if isinstance(path, str) and path:
        return path.split("@", 1)[0]
    workflow_id = _as_int(run.get("workflow_id"))
    if workflow_id is not None:
        return f"workflow:{workflow_id}"
    return f"run:{run.get('id')}"


def _is_cancelled(check: dict) -> bool:
    return _lower(check.get("status")) == "completed" and _lower(check.get("conclusion")) == (
        "cancelled"
    )


def _is_skipped(check: dict) -> bool:
    return _lower(check.get("status")) == "completed" and _lower(check.get("conclusion")) == (
        "skipped"
    )


def _effective(entries: list[tuple[dict, dict | None]]) -> tuple[dict, dict | None]:
    """The entry that stands for a group, in check-run id order.

    A cancelled check is replaced by anything newer. A newer cancellation
    never hides an older result, and a newer skip never hides an older result
    that actually ran.
    """
    ordered = sorted(entries, key=lambda entry: entry[0]["id"])
    current = ordered[0]
    for entry in ordered[1:]:
        check = entry[0]
        if _is_cancelled(current[0]):
            current = entry
        elif _is_cancelled(check):
            continue
        elif _is_skipped(check) and not _is_skipped(current[0]):
            continue
        else:
            current = entry
    return current


def judge_check_runs(
    check_runs: list[dict],
    workflow_runs: list[dict],
    head_sha: str,
    required: set[str] | None,
    *,
    statuses: list[dict] | None = None,
    head_branch: str | None = None,
    require_re: re.Pattern[str] | None = None,
    runs_grace_elapsed: bool = False,
) -> Verdict:
    """Judge the head commit's checks. Pure: no network, no clock.

    `check_runs` are `repos/{r}/commits/{sha}/check-runs` items and
    `workflow_runs` are `actions/runs?head_sha=` items, all pages. See the
    module docstring for the supersession rule this implements.

    `runs_grace_elapsed` says the head commit has had no workflow run for the
    whole grace period: a missing required check then never reports (#1418)
    instead of staying pending.
    """

    def is_required(name: str) -> bool:
        if require_re is not None:
            return bool(require_re.search(name))
        if not required:
            return True
        return name in required

    runs_by_id: dict[int, dict] = {}
    runs_by_suite: dict[int, dict] = {}
    for run in workflow_runs:
        if not isinstance(run, dict):
            continue
        run_id = _as_int(run.get("id"))
        if run_id is None:
            continue
        runs_by_id[run_id] = run
        suite = _as_int(run.get("check_suite_id"))
        if suite is not None:
            runs_by_suite[suite] = run

    def on_head(run: dict) -> bool:
        if run.get("head_sha") != head_sha or run.get("event") == "merge_group":
            return False
        branch = run.get("head_branch")
        return not (head_branch and branch and branch != head_branch)

    head_runs = [run for run in runs_by_id.values() if on_head(run)]

    def newer_runs(key: str, run_id: int) -> list[dict]:
        return [r for r in head_runs if _workflow_key(r) == key and r["id"] > run_id]

    groups: dict[tuple[str, str], list[tuple[dict, dict | None]]] = {}
    for check in check_runs:
        if not isinstance(check, dict) or check.get("head_sha") != head_sha:
            continue
        name = check.get("name")
        if _as_int(check.get("id")) is None or not isinstance(name, str):
            continue
        run: dict | None = None
        linked = _extract_run_id_from_link(check.get("details_url") or check.get("html_url"))
        if linked:
            run = runs_by_id.get(int(linked))
        if run is None:
            suite = _as_int((check.get("check_suite") or {}).get("id"))
            if suite is not None:
                run = runs_by_suite.get(suite)
        if run is not None:
            if not on_head(run):
                continue
            key = _workflow_key(run)
        else:
            key = f"app:{(check.get('app') or {}).get('slug') or 'unknown'}"
        groups.setdefault((key, name), []).append((check, run))

    judgments: list[CheckJudgment] = []
    notes: list[str] = []
    for (key, name), entries in groups.items():
        events = {str(run.get("event")) for _check, run in entries if run is not None}
        if len(events) > 1:
            notes.append(
                f"{name} ({key}) reported by runs from events {sorted(events)}; "
                "the newest check counts"
            )
        check, run = _effective(entries)
        status = _lower(check.get("status"))
        conclusion = _lower(check.get("conclusion"))
        run_id = _as_int(run.get("id")) if run is not None else None
        cancelled = False
        if status != "completed":
            state = "pending"
        elif conclusion in PASSING_CONCLUSIONS:
            state = "pass"
        elif conclusion == "action_required":
            state = "approval_required"
        elif conclusion == "stale":
            state = "stale"
        elif conclusion == "cancelled":
            cancelled = True
            newer = newer_runs(key, run_id) if run_id is not None else []
            if run is not None and _lower(run.get("status")) != "completed":
                state = "pending"  # its own run is re-running
            elif not newer:
                state = "fail"
            elif any(_lower(r.get("status")) != "completed" for r in newer):
                state = "pending"
            else:
                state = "superseded"
        else:
            state = "fail"
            # An `if: always()` gate fails because its run was cancelled; a
            # newer run of the workflow supersedes it.
            if run is not None and run_id is not None and _lower(run.get("conclusion")) == (
                "cancelled"
            ):
                newer = newer_runs(key, run_id)
                if newer:
                    state = (
                        "pending"
                        if any(_lower(r.get("status")) != "completed" for r in newer)
                        else "superseded"
                    )
        judgments.append(
            CheckJudgment(
                name=name,
                workflow=key,
                state=state,
                conclusion=conclusion or status,
                check_run_id=_as_int(check.get("id")),
                run_id=run_id,
                link=check.get("details_url") or check.get("html_url") or None,
                required=is_required(name),
                workflow_broken=conclusion == "startup_failure",
                cancelled=cancelled,
            )
        )

    # Run-level results that produce no check runs at all.
    for run in head_runs:
        if _lower(run.get("status")) != "completed":
            continue
        conclusion = _lower(run.get("conclusion"))
        if conclusion not in {"startup_failure", "action_required"}:
            continue
        key = _workflow_key(run)
        if newer_runs(key, run["id"]) or any(j.run_id == run["id"] for j in judgments):
            continue
        judgments.append(
            CheckJudgment(
                name=str(run.get("name") or key),
                workflow=key,
                state="fail" if conclusion == "startup_failure" else "approval_required",
                conclusion=conclusion,
                check_run_id=None,
                run_id=run["id"],
                link=run.get("html_url") or None,
                required=True,
                workflow_broken=conclusion == "startup_failure",
            )
        )

    # Legacy commit statuses: newest per context (highest id when present).
    seen_contexts: set[str] = set()
    for status_item in sorted(
        (s for s in statuses or [] if isinstance(s, dict)),
        key=lambda s: -(_as_int(s.get("id")) or 0),
    ):
        context = status_item.get("context")
        if not isinstance(context, str) or context in seen_contexts:
            continue
        seen_contexts.add(context)
        raw = _lower(status_item.get("state"))
        state = (
            "pass" if raw == "success" else "pending" if raw in {"pending", "expected"} else "fail"
        )
        judgments.append(
            CheckJudgment(
                name=context,
                workflow="status",
                state=state,
                conclusion=raw,
                check_run_id=None,
                run_id=None,
                link=status_item.get("target_url") or None,
                required=is_required(context),
            )
        )

    missing: list[str] = []
    if required and require_re is None:
        reported = {j.name for j in judgments}
        missing = sorted(required - reported)
    all_runs_done = bool(head_runs) and all(
        _lower(r.get("status")) == "completed" for r in head_runs
    )

    req = [j for j in judgments if j.required]
    real_failure_runs = {
        j.run_id for j in req if j.state == "fail" and not j.cancelled and j.run_id is not None
    }
    # Fail-fast siblings of a real failure in the same run are not the cause.
    failing = [
        j
        for j in req
        if j.state == "fail" and not (j.cancelled and j.run_id in real_failure_runs)
    ]
    advisory = [j for j in judgments if not j.required and j.state in {"fail", "stale"}]
    failing_run_ids: dict[str, int] = {}
    for j in failing:
        if j.run_id is not None and j.workflow not in {"status"}:
            failing_run_ids[j.workflow] = max(failing_run_ids.get(j.workflow, 0), j.run_id)

    if any(j.state == "approval_required" for j in req):
        state = "approval_required"
    elif failing:
        state = "fail"
    elif any(j.state == "stale" for j in req):
        state = "stale"
    elif any(j.state == "pending" for j in req):
        state = "pending"
    elif missing:
        no_runs_ever = not head_runs and runs_grace_elapsed
        state = "never_reported" if all_runs_done or no_runs_ever else "pending"
    elif not judgments:
        state = "pass" if all_runs_done else "pending"
    else:
        state = "pass"
    return Verdict(state, judgments, failing, advisory, failing_run_ids, missing, notes)


def paginate(path: str, key: str) -> list[dict] | None:
    """Every page of a REST list endpoint; None if any page is unreadable."""
    sep = "&" if "?" in path else "?"
    items: list[dict] = []
    for page in range(1, MAX_PAGES + 1):
        data = gh_json("api", f"{path}{sep}per_page={PER_PAGE}&page={page}")
        if not isinstance(data, dict) or not isinstance(data.get(key), list):
            return None
        batch = [item for item in data[key] if isinstance(item, dict)]
        items.extend(batch)
        if len(data[key]) < PER_PAGE:
            break
    return items


def fetch_head_checks(
    repo: str, head_sha: str, statuses: list[dict] | None = None
) -> HeadChecks | None:
    """All check runs and workflow runs for one commit, every page."""
    if not head_sha:
        return None
    check_runs = paginate(f"repos/{repo}/commits/{head_sha}/check-runs?filter=all", "check_runs")
    if check_runs is None:
        return None
    runs = paginate(f"repos/{repo}/actions/runs?head_sha={head_sha}", "workflow_runs")
    if runs is None:
        return None
    return HeadChecks(check_runs, runs, list(statuses or []))


def fetch_checks(pr: int, repo: str | None) -> list[CheckRow] | None:
    args = ["pr", "checks", str(pr), "--json", "name,bucket,state,link"]
    if repo:
        args = ["pr", "checks", str(pr), "--repo", repo, "--json", "name,bucket,state,link"]
    # `gh pr checks` returns exit 1 when ANY check failed even with --json;
    # capture stdout regardless.
    res = gh(*args)
    if not res.stdout.strip():
        if re.search(r"no checks reported", res.stderr, re.IGNORECASE):
            return []
        return None
    try:
        rows = json.loads(res.stdout)
    except json.JSONDecodeError:
        return None
    if not isinstance(rows, list):
        return None
    return [
        CheckRow(
            name=str(r.get("name", "")),
            bucket=str(r.get("bucket", "pending")),
            state=str(r.get("state", "")),
            link=r.get("link") or None,
        )
        for r in rows
    ]


def fetch_required_check_names(repo: str, base_ref: str) -> set[str] | None:
    """Read the base branch's required-status-checks protection.

    Returns:
        - set of required check names if branch protection is configured;
        - empty set if protection exists but lists no checks;
        - None if the caller lacks permission OR no protection is set
          (callers should then fall back to --require allowlist).
    """
    data = gh_json("api", f"repos/{repo}/branches/{base_ref}/protection/required_status_checks")
    if not isinstance(data, dict):
        return None
    contexts = data.get("contexts")
    required = {str(context) for context in contexts} if isinstance(contexts, list) else set()
    checks = data.get("checks")
    if isinstance(checks, list):
        required.update(
            str(check["context"])
            for check in checks
            if isinstance(check, dict) and isinstance(check.get("context"), str)
        )
    return required


# One bounded probe. It runs on the fail-fast path, ahead of cancellation,
# so it must never be what delays either.
LOG_PROBE_TIMEOUT_SEC = 25.0


def fetch_failure_log(repo: str, run_id: str | None, job_id: str | None) -> str:
    """The failing job's log, normalized, or `""` if it cannot be read now.

    Prefers the REST **job** endpoint. `gh run view --log-failed` resolves the
    whole *run*, and GitHub refuses that while any job is still going —
    `run … is still in progress; logs will be available when it is complete`.
    On the fail-fast path the run is in progress by definition, so the
    run-level probe returns nothing exactly when the caller needs it most.
    The job endpoint serves a finished job's log regardless of its siblings.
    """
    if job_id:
        res = gh(
            "api",
            f"repos/{repo}/actions/jobs/{job_id}/logs",
            "--allow-escape-sequences",
            timeout=LOG_PROBE_TIMEOUT_SEC,
        )
        if res.ok and res.stdout.strip():
            return normalize_log(strip_ansi(res.stdout))
    if not run_id:
        return ""
    res = gh(
        "run", "view", run_id, "--repo", repo, "--log-failed",
        timeout=LOG_PROBE_TIMEOUT_SEC,
    )
    return normalize_log(strip_ansi(res.stdout)) if res.ok else ""


ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def strip_ansi(text: str) -> str:
    return ANSI_ESCAPE.sub("", text)


# Ordered most- to least-specific. `##[error]` first because when a step
# emits one it is by construction the reason the step failed; the rest are
# what a build or test harness prints on its way there.
FIRST_ERROR_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"^##\[error\]"),
    re.compile(r"^FAILED \S"),
    re.compile(r"^thread .*? panicked at"),
    re.compile(r"^error(\[E\d+\])?:"),
    re.compile(r"^Error:"),
    re.compile(r"^Diff in "),
]


def first_error_line(sample: str) -> str:
    """The first line that names why the job failed, or `""`.

    Passing cargo test lines are skipped explicitly rather than filtered by
    pattern precision, because a test may legitimately be *named* after the
    failure mode it guards ("..._not_panicked_on") and no error pattern can
    tell that apart from a real panic by content alone.
    """
    for raw in sample.splitlines():
        line = raw.strip()
        if not line or CARGO_TEST_OUTCOME.match(line):
            continue
        for pattern in FIRST_ERROR_PATTERNS:
            if pattern.search(line):
                return GHA_ANNOTATION.sub("", line).strip()
    return ""


def classify_failure(
    repo: str, run_id: str | None, job_id: str | None
) -> tuple[str, str | None]:
    """Read the failing job's log and classify the first error.

    Returns (first_error_line, classifier_label).
    """
    sample = fetch_failure_log(repo, run_id, job_id)
    if not sample:
        return "", None
    first_err = first_error_line(sample)
    label = None
    for pattern, lbl in CLASSIFIERS:
        if pattern.search(sample):
            label = lbl
            break
    return first_err, label


@dataclass
class FailureReport:
    check: CheckRow
    run_id: str | None
    first_error: str
    classifier: str | None

    def render(self) -> str:
        lines = [
            f"FAIL  {self.check.name}",
            f"  conclusion: {self.check.state or self.check.bucket}",
        ]
        if self.check.link:
            lines.append(f"  link:       {self.check.link}")
        if self.run_id:
            lines.append(f"  log probe:  gh run view {self.run_id} --log-failed | tail -100")
        if self.first_error:
            lines.append(f"  first error: {self.first_error}")
        if self.classifier:
            lines.append(f"  classifier: {self.classifier}")
        return "\n".join(lines)


# ---------- review activity ---------------------------------------------------

CODERABBIT_LOGINS = {"coderabbitai", "coderabbitai[bot]", "coderabbit[bot]"}


def _is_coderabbit(login: object) -> bool:
    return isinstance(login, str) and login.lower() in CODERABBIT_LOGINS


@dataclass(frozen=True)
class CodeRabbitProbe:
    state: str  # detected | not_detected | degraded
    sampled_merged_prs: int


@dataclass(frozen=True)
class CodeRabbitObservation:
    state: str  # quiet | skipped | actionable | degraded
    reason: str | None = None
    actionable: bool = False
    unresolved_threads: int = 0
    ids: frozenset[int] = frozenset()


def probe_coderabbit(repo: str) -> CodeRabbitProbe:
    recent = gh_json(
        "pr", "list", "--repo", repo, "--state", "merged", "--limit", "5", "--json", "number"
    )
    if not isinstance(recent, list):
        return CodeRabbitProbe("degraded", 0)
    sampled = 0
    for item in recent:
        if not isinstance(item, dict) or not isinstance(item.get("number"), int):
            continue
        sampled += 1
        reviews = gh_json("api", f"repos/{repo}/pulls/{item['number']}/reviews?per_page=100")
        if not isinstance(reviews, list):
            return CodeRabbitProbe("degraded", sampled)
        if any(
            isinstance(review, dict) and _is_coderabbit((review.get("user") or {}).get("login"))
            for review in reviews
        ):
            return CodeRabbitProbe("detected", sampled)
    return CodeRabbitProbe("not_detected", sampled)


def classify_coderabbit(
    threads: list[dict],
    status_comments: list[dict],
) -> CodeRabbitObservation:
    ids: set[int] = set()
    unresolved = 0
    for thread in threads:
        if not isinstance(thread, dict) or thread.get("isResolved") is True:
            continue
        nodes = (thread.get("comments") or {}).get("nodes", [])
        if not isinstance(nodes, list):
            continue
        coderabbit_comments = [
            comment
            for comment in nodes
            if isinstance(comment, dict)
            and _is_coderabbit((comment.get("author") or {}).get("login"))
        ]
        if not coderabbit_comments:
            continue
        unresolved += 1
        for comment in coderabbit_comments:
            if isinstance(comment.get("databaseId"), int):
                ids.add(comment["databaseId"])
    if unresolved:
        return CodeRabbitObservation(
            "actionable",
            actionable=True,
            unresolved_threads=unresolved,
            ids=frozenset(ids),
        )

    for comment in reversed(status_comments):
        if not isinstance(comment, dict):
            continue
        if not _is_coderabbit((comment.get("user") or {}).get("login")):
            continue
        body = str(comment.get("body", ""))
        credit_subject = r"credits?|quota|usage\s+limit"
        unavailable = r"exhaust(?:ed|ion)?|out\s+of|deplet(?:ed|ion)?|paused|unavailable"
        credits = re.search(
            rf"(?:{credit_subject}).{{0,100}}(?:{unavailable})|"
            rf"(?:{unavailable}).{{0,100}}(?:{credit_subject})",
            body,
            re.IGNORECASE | re.DOTALL,
        )
        if credits:
            return CodeRabbitObservation("skipped", reason="credits_exhausted")
        if re.search(r"review\s+skipped", body, re.IGNORECASE):
            return CodeRabbitObservation("skipped", reason="review_skipped")
    return CodeRabbitObservation("quiet")


@dataclass(frozen=True)
class GateSnapshot:
    pr: PRSnapshot
    checks: list[CheckRow]
    human_review_ids: frozenset[int]
    coderabbit_probe: CodeRabbitProbe | None
    coderabbit: CodeRabbitObservation | None
    # REST check runs + workflow runs for the head commit (#1330); None when
    # unavailable, in which case the rollup rows above are judged instead.
    head_checks: HeadChecks | None = None


def _rollup_check(node: dict) -> CheckRow | None:
    kind = node.get("__typename")
    if kind == "CheckRun":
        name = str(node.get("name", ""))
        status = str(node.get("status", "")).upper()
        conclusion = str(node.get("conclusion") or "").upper()
        if status != "COMPLETED":
            bucket = "pending"
        elif conclusion in {"SUCCESS", "NEUTRAL"}:
            bucket = "pass"
        elif conclusion == "SKIPPED":
            bucket = "skipping"
        elif conclusion == "CANCELLED":
            bucket = "cancel"
        else:
            bucket = "fail"
        return CheckRow(name, bucket, conclusion or status, node.get("detailsUrl") or None)
    if kind == "StatusContext":
        name = str(node.get("context", ""))
        state = str(node.get("state", "")).upper()
        bucket = (
            "pass"
            if state == "SUCCESS"
            else "pending"
            if state in {"PENDING", "EXPECTED"}
            else "fail"
        )
        return CheckRow(name, bucket, state, node.get("targetUrl") or None)
    return None


def _connection_truncated(connection: object, *, from_end: bool = False) -> bool:
    if not isinstance(connection, dict):
        return True
    page_info = connection.get("pageInfo")
    if not isinstance(page_info, dict):
        return True
    key = "hasPreviousPage" if from_end else "hasNextPage"
    value = page_info.get(key)
    return not isinstance(value, bool) or value


def fetch_gate_snapshot(repo: str, pr: int, *, include_coderabbit: bool) -> GateSnapshot | None:
    owner, separator, name = repo.partition("/")
    if not separator or not owner or not name:
        return None
    query = """
query($owner:String!,$name:String!,$number:Int!,$includeCoderabbit:Boolean!){
  repository(owner:$owner,name:$name){
    pullRequest(number:$number){
      number state mergeable mergeStateStatus headRefOid baseRefName headRefName
      reviews(first:100){nodes{databaseId state author{login}} pageInfo{hasNextPage}}
      reviewThreads(first:100) @include(if:$includeCoderabbit){
        nodes{isResolved comments(first:20){
          nodes{databaseId body author{login}} pageInfo{hasNextPage}
        }}
        pageInfo{hasNextPage}
      }
      comments(last:100) @include(if:$includeCoderabbit){
        nodes{body author{login}} pageInfo{hasPreviousPage}
      }
      commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){nodes{
        __typename
        ... on CheckRun{name status conclusion detailsUrl}
        ... on StatusContext{context state targetUrl}
      } pageInfo{hasNextPage}}}}}}
    }
    recent:pullRequests(first:5,states:MERGED,orderBy:{field:UPDATED_AT,direction:DESC})
      @include(if:$includeCoderabbit){
      nodes{number reviews(first:100){nodes{author{login}} pageInfo{hasNextPage}}}
    }
  }
}
"""
    data = gh_json(
        "api",
        "graphql",
        "-f",
        f"query={query}",
        "-F",
        f"owner={owner}",
        "-F",
        f"name={name}",
        "-F",
        f"number={pr}",
        "-F",
        f"includeCoderabbit={'true' if include_coderabbit else 'false'}",
    )
    repository = (
        ((data or {}).get("data") or {}).get("repository") if isinstance(data, dict) else None
    )
    pull = repository.get("pullRequest") if isinstance(repository, dict) else None
    if not isinstance(pull, dict):
        return None

    reviews_connection = pull.get("reviews")
    if _connection_truncated(reviews_connection):
        return None

    commit_nodes = (pull.get("commits") or {}).get("nodes") or []
    rollup_nodes: list[dict] = []
    if isinstance(commit_nodes, list) and commit_nodes and isinstance(commit_nodes[-1], dict):
        commit = commit_nodes[-1].get("commit")
        if not isinstance(commit, dict):
            return None
        rollup = commit.get("statusCheckRollup")
        if rollup is not None:
            if not isinstance(rollup, dict):
                return None
            contexts = rollup.get("contexts")
            if _connection_truncated(contexts):
                return None
            candidate_nodes = contexts.get("nodes")
            if not isinstance(candidate_nodes, list):
                return None
            rollup_nodes = [node for node in candidate_nodes if isinstance(node, dict)]
    else:
        return None
    checks = [row for node in rollup_nodes if (row := _rollup_check(node)) is not None]
    # GitHub already keeps only the newest status per context in the rollup.
    statuses = [
        {
            "context": node.get("context"),
            "state": str(node.get("state", "")).lower(),
            "target_url": node.get("targetUrl"),
        }
        for node in rollup_nodes
        if node.get("__typename") == "StatusContext"
    ]

    review_nodes = reviews_connection.get("nodes") or []
    human_ids = (
        frozenset(
            review["databaseId"]
            for review in review_nodes
            if isinstance(review, dict)
            and isinstance(review.get("databaseId"), int)
            and "[bot]" not in str((review.get("author") or {}).get("login", ""))
            and review.get("state") in {"CHANGES_REQUESTED", "COMMENTED"}
        )
        if isinstance(review_nodes, list)
        else frozenset()
    )

    probe: CodeRabbitProbe | None = None
    observation: CodeRabbitObservation | None = None
    if include_coderabbit:
        threads_connection = pull.get("reviewThreads")
        comments_connection = pull.get("comments")
        if _connection_truncated(threads_connection) or _connection_truncated(
            comments_connection, from_end=True
        ):
            return None
        recent = repository.get("recent")
        recent_nodes = (recent.get("nodes") or []) if isinstance(recent, dict) else None
        sampled = 0
        detected = False
        if isinstance(recent_nodes, list):
            for recent in recent_nodes:
                if not isinstance(recent, dict) or not isinstance(recent.get("number"), int):
                    continue
                sampled += 1
                recent_reviews = recent.get("reviews")
                if _connection_truncated(recent_reviews):
                    return None
                reviews = recent_reviews.get("nodes") or []
                if isinstance(reviews, list) and any(
                    isinstance(review, dict)
                    and _is_coderabbit((review.get("author") or {}).get("login"))
                    for review in reviews
                ):
                    detected = True
                    break
            probe = CodeRabbitProbe("detected" if detected else "not_detected", sampled)
        else:
            probe = CodeRabbitProbe("degraded", 0)
        threads = threads_connection.get("nodes") or []
        comments = comments_connection.get("nodes") or []
        if not isinstance(threads, list) or not isinstance(comments, list):
            observation = CodeRabbitObservation("degraded", reason="malformed_payload")
        else:
            if any(
                not isinstance(thread, dict)
                or _connection_truncated(thread.get("comments"))
                for thread in threads
            ):
                return None
            normalized_comments = [
                {"user": comment.get("author") or {}, "body": comment.get("body", "")}
                for comment in comments
                if isinstance(comment, dict)
            ]
            observation = classify_coderabbit(threads, normalized_comments)

    return GateSnapshot(
        pr=PRSnapshot(
            number=int(pull.get("number", pr)),
            state=str(pull.get("state", "UNKNOWN")),
            mergeable=str(pull.get("mergeable", "UNKNOWN")),
            head_sha=str(pull.get("headRefOid", "")),
            base_ref=str(pull.get("baseRefName", "main")),
            head_ref=str(pull.get("headRefName") or ""),
            merge_state=str(pull.get("mergeStateStatus") or "UNKNOWN"),
        ),
        checks=checks,
        human_review_ids=human_ids,
        coderabbit_probe=probe,
        coderabbit=observation,
        head_checks=fetch_head_checks(repo, str(pull.get("headRefOid", "")), statuses),
    )


def fetch_coderabbit(repo: str, pr: int) -> CodeRabbitObservation:
    owner, separator, name = repo.partition("/")
    if not separator or not owner or not name:
        return CodeRabbitObservation("degraded", reason="invalid_repo")
    query = """
query($owner:String!,$name:String!,$number:Int!){
  repository(owner:$owner,name:$name){
    pullRequest(number:$number){
      reviewThreads(first:100){
        nodes{isResolved comments(first:20){nodes{databaseId body author{login}}}}
      }
    }
  }
}
"""
    thread_data = gh_json(
        "api",
        "graphql",
        "-f",
        f"query={query}",
        "-F",
        f"owner={owner}",
        "-F",
        f"name={name}",
        "-F",
        f"number={pr}",
    )
    comments = gh_json("api", f"repos/{repo}/issues/{pr}/comments?per_page=100")
    if not isinstance(thread_data, dict) or not isinstance(comments, list):
        return CodeRabbitObservation("degraded", reason="api_error")
    threads = (
        (((thread_data.get("data") or {}).get("repository") or {}).get("pullRequest") or {})
        .get("reviewThreads", {})
        .get("nodes", [])
    )
    if not isinstance(threads, list):
        return CodeRabbitObservation("degraded", reason="malformed_payload")
    return classify_coderabbit(threads, comments)


@dataclass
class ReviewState:
    """Tracks new human reviews and actionable CodeRabbit threads."""

    coderabbit_enabled: bool = False
    seen_review_ids: set[int] = field(default_factory=set)
    seen_coderabbit_ids: set[int] = field(default_factory=set)
    initialized: bool = False
    last_coderabbit_state: tuple[str, str | None] | None = None

    def update(self, pr: int, repo: str | None, log: WatchLog | None = None) -> bool:
        repo_arg = repo or _resolve_origin_repo()
        if not repo_arg:
            if log:
                log.emit("api_degraded", source="reviews", reason="repo_unresolved")
            return False
        reviews = gh_json("api", f"repos/{repo_arg}/pulls/{pr}/reviews?per_page=100")
        human_ids: set[int] = set()
        if isinstance(reviews, list):
            for review in reviews:
                if not isinstance(review, dict):
                    continue
                user = str((review.get("user") or {}).get("login", ""))
                rid = review.get("id")
                if (
                    isinstance(rid, int)
                    and "[bot]" not in user
                    and review.get("state") in {"CHANGES_REQUESTED", "COMMENTED"}
                ):
                    human_ids.add(rid)
        elif log:
            log.emit("api_degraded", source="reviews", reason="api_error")

        observation = fetch_coderabbit(repo_arg, pr) if self.coderabbit_enabled else None
        return self.update_prefetched(frozenset(human_ids), observation, log)

    def update_prefetched(
        self,
        human_ids: frozenset[int],
        observation: CodeRabbitObservation | None,
        log: WatchLog | None = None,
    ) -> bool:
        coderabbit_actionable = False
        if self.coderabbit_enabled and observation is not None:
            signature = (observation.state, observation.reason)
            if signature != self.last_coderabbit_state and log:
                payload: dict[str, object] = {"state": observation.state}
                if observation.reason:
                    payload["reason"] = observation.reason
                if observation.unresolved_threads:
                    payload["unresolved_threads"] = observation.unresolved_threads
                log.emit("coderabbit", coderabbit=payload)
            self.last_coderabbit_state = signature
            if observation.state == "skipped":
                self.coderabbit_enabled = False
            if observation.actionable:
                new_ids = set(observation.ids) - self.seen_coderabbit_ids
                coderabbit_actionable = bool(new_ids) or not self.initialized
                self.seen_coderabbit_ids.update(observation.ids)

        if not self.initialized:
            self.seen_review_ids = set(human_ids)
            self.initialized = True
            return coderabbit_actionable
        new_human = set(human_ids) - self.seen_review_ids
        self.seen_review_ids = set(human_ids)
        return bool(new_human) or coderabbit_actionable


# ---------- cancellation ------------------------------------------------------


@dataclass
class CancelOptions:
    on: set[str]
    mode: str  # runs | jobs | none
    timeout: int
    require: bool
    dry_run: bool
    ignore_permission_errors: bool
    no_retry: bool
    # The head SHA the watch started on (#1418); runs on any other SHA are
    # never cancelled, so an orphaned watch cannot kill a fix's fresh CI.
    pinned_sha: str | None = None


def cancel_pr_runs(
    pr: int,
    repo: str | None,
    head_sha: str,
    opts: CancelOptions,
    log: WatchLog | None = None,
    *,
    scope: dict[str, int] | None = None,
) -> int:
    """Cancel non-completed workflow runs on the PR's head SHA.

    `scope` (workflow key -> failing run id) limits cancellation to the
    failing workflows' runs at or below the failing run: never a newer run,
    never another workflow (#1330).

    Returns the number of cancel attempts. Failures are surfaced as
    `CANCEL <id> status=…` lines on stdout.
    """
    if opts.mode == "none":
        return 0
    if not head_sha:
        if log:
            log.emit("cancel_item", status="skipped", reason="head_sha_missing")
        return 0
    repo_arg = repo if repo else _resolve_origin_repo()
    if not repo_arg:
        print(f"CANCEL  skipped: could not resolve repo for PR #{pr}")
        if log:
            log.emit("cancel_item", status="skipped", reason="repo_unresolved")
        return 0
    runs = paginate(f"repos/{repo_arg}/actions/runs?head_sha={head_sha}", "workflow_runs")
    if runs is None:
        if log:
            log.emit("api_degraded", source="cancel_runs", reason="fetch_failed")
        return 0
    attempts = 0
    for r in runs:
        if not isinstance(r, dict):
            continue
        rid = r.get("id")
        status = r.get("status", "")
        if status in {"completed", "cancelled"} or not isinstance(rid, int):
            continue
        run_head_sha = r.get("head_sha")
        if not isinstance(run_head_sha, str) or run_head_sha != head_sha:
            if log:
                log.emit(
                    "cancel_item",
                    mode="runs",
                    run_id=rid,
                    status="skipped",
                    reason=("head_sha_mismatch" if run_head_sha else "head_sha_missing"),
                )
            continue
        if scope is not None:
            limit = scope.get(_workflow_key(r))
            if limit is None or rid > limit:
                if log:
                    log.emit(
                        "cancel_item",
                        mode=opts.mode,
                        run_id=rid,
                        status="skipped",
                        reason="out_of_scope",
                    )
                continue
        if opts.mode == "runs":
            attempts += 1
            if opts.dry_run:
                print(f"CANCEL  workflow_run={rid} status=DRY-RUN")
                if log:
                    log.emit("cancel_item", mode="runs", run_id=rid, status="dry_run")
                continue
            cancel = gh("api", "-X", "POST", f"repos/{repo_arg}/actions/runs/{rid}/cancel")
            _report_cancel(rid, cancel, opts, log, "runs")
        elif opts.mode == "jobs":
            jobs_resp = gh_json("api", f"repos/{repo_arg}/actions/runs/{rid}/jobs?per_page=100")
            if not isinstance(jobs_resp, dict):
                if log:
                    log.emit(
                        "api_degraded",
                        source="cancel_jobs",
                        reason="fetch_failed",
                        run_id=rid,
                    )
                continue
            jobs = jobs_resp.get("jobs", [])
            if not isinstance(jobs, list):
                if log:
                    log.emit(
                        "api_degraded",
                        source="cancel_jobs",
                        reason="malformed_payload",
                        run_id=rid,
                    )
                continue
            for j in jobs:
                if not isinstance(j, dict) or j.get("status") == "completed":
                    continue
                jid = j.get("id")
                if not isinstance(jid, int):
                    continue
                attempts += 1
                if opts.dry_run:
                    print(f"CANCEL  job={jid} status=DRY-RUN")
                    if log:
                        log.emit("cancel_item", mode="jobs", item_id=jid, status="dry_run")
                    continue
                cancel = gh("api", "-X", "POST", f"repos/{repo_arg}/actions/jobs/{jid}/cancel")
                _report_cancel(jid, cancel, opts, log, "jobs")
    return attempts


def _report_cancel(
    item_id: int,
    res: GhResult,
    opts: CancelOptions,
    log: WatchLog | None,
    mode: str,
) -> None:
    if res.ok:
        print(f"CANCEL  id={item_id} status=cancelled")
        if log:
            log.emit("cancel_item", mode=mode, item_id=item_id, status="cancelled")
        return
    stderr = res.stderr or ""
    err = stderr.strip().splitlines()
    err_first = err[0] if err else "unknown"
    if "HTTP 403" in stderr or "Resource not accessible" in stderr:
        print(f"CANCEL  id={item_id} status=permission_denied  ({err_first})")
        if log:
            log.emit(
                "cancel_item",
                mode=mode,
                item_id=item_id,
                status="permission_denied",
                required=opts.require,
            )
    elif "HTTP 404" in stderr or "HTTP 422" in stderr:
        print(f"CANCEL  id={item_id} status=already_completed")
        if log:
            log.emit("cancel_item", mode=mode, item_id=item_id, status="already_completed")
    else:
        print(f"CANCEL  id={item_id} status=error  ({err_first})")
        if log:
            log.emit(
                "cancel_item",
                mode=mode,
                item_id=item_id,
                status="error",
                required=opts.require,
            )


def _resolve_origin_repo() -> str | None:
    res = gh("repo", "view", "--json", "nameWithOwner")
    if not res.ok:
        return None
    try:
        return json.loads(res.stdout).get("nameWithOwner")
    except json.JSONDecodeError:
        return None


# ---------- main poll loop ----------------------------------------------------


def _exit_after_cancel(
    code: int,
    on_label: str,
    pr: int,
    repo: str | None,
    head_sha: str,
    opts: CancelOptions,
    log: WatchLog | None = None,
) -> None:
    """Cancel (if enabled) then exit `code`."""
    _cancel_for_exit(on_label, pr, repo, head_sha, opts, log)
    _finish_exit(code, on_label, log)


def _cancel_for_exit(
    on_label: str,
    pr: int,
    repo: str | None,
    head_sha: str,
    opts: CancelOptions,
    log: WatchLog | None = None,
    scope: dict[str, int] | None = None,
) -> None:
    if on_label in opts.on or "always" in opts.on:
        if opts.pinned_sha and head_sha != opts.pinned_sha:
            print(
                f"NOTE  head moved from {opts.pinned_sha[:7]} to {head_sha[:7]}; "
                "not cancelling runs this watch did not start on",
                file=sys.stderr,
            )
            if log:
                log.emit(
                    "cancel_skipped", reason="head_moved", pinned=opts.pinned_sha, head=head_sha
                )
            return
        if scope is None:
            attempts = cancel_pr_runs(pr, repo, head_sha, opts, log)
        else:
            attempts = cancel_pr_runs(pr, repo, head_sha, opts, log, scope=scope)
        if log:
            log.emit("cancel", trigger=on_label, attempts=attempts, mode=opts.mode)


def _finish_exit(code: int, reason: str, log: WatchLog | None = None, **fields: object) -> None:
    if log:
        log.emit("EXIT", code=code, reason=reason, **fields)
        log.close()
    sys.exit(code)


def _exit_unreachable(why: str, log: WatchLog | None) -> None:
    detail = LAST_GH_ERROR or "(no gh stderr captured)"
    print(f"GITHUB-UNREACHABLE  {why}: {detail}", file=sys.stderr)
    _finish_exit(EXIT_GITHUB_UNREACHABLE, "github_unreachable", log, why=why, stderr=detail)


class WatchKilled(Exception):
    """SIGINT/SIGTERM arrived; `main` writes the final `EXIT` event."""

    def __init__(self, signum: int) -> None:
        super().__init__(f"signal {signum}")
        self.signum = signum
        self.code = 128 + signum


def install_kill_handlers() -> None:
    """On SIGINT/SIGTERM unwind to `main`, which writes a final `EXIT` event
    and cancels nothing: the caller that killed the watch did not ask for
    cancellation (#1418)."""

    def on_signal(signum: int, _frame: object) -> None:
        raise WatchKilled(int(signum))

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, on_signal)


def fetch_workflow_presence(repo: str, ref: str) -> bool | None:
    """True/False when the base branch does/doesn't have workflow files;
    None when GitHub could not be asked."""
    res = gh("api", f"repos/{repo}/contents/.github/workflows?ref={ref}")
    if not res.ok:
        return False if "HTTP 404" in res.stderr else None
    try:
        entries = json.loads(res.stdout)
    except json.JSONDecodeError:
        return None
    if not isinstance(entries, list):
        return None
    return any(
        isinstance(e, dict) and str(e.get("name", "")).endswith((".yml", ".yaml"))
        for e in entries
    )


def fetch_head_commit_message(repo: str, sha: str) -> str | None:
    data = gh_json("api", f"repos/{repo}/commits/{sha}")
    if not isinstance(data, dict):
        return None
    message = (data.get("commit") or {}).get("message")
    return message if isinstance(message, str) else None


def immediate_no_checks_reason(repo: str, snapshot: PRSnapshot) -> str | None:
    """Why no check can ever report on this head, known without waiting."""
    # A PR adding the repo's first workflow runs it from its own ref, so the
    # repo has no workflows only when neither side does.
    if (
        fetch_workflow_presence(repo, snapshot.base_ref) is False
        and fetch_workflow_presence(repo, snapshot.head_sha) is False
    ):
        return "no_workflows"
    message = fetch_head_commit_message(repo, snapshot.head_sha)
    if message and SKIP_CI_MARKER.search(message):
        return "skip_ci_marker"
    return None


def fetch_queued_jobs(repo: str, run_id: int) -> list[dict]:
    data = gh_json("api", f"repos/{repo}/actions/runs/{run_id}/jobs?per_page=100")
    jobs = data.get("jobs") if isinstance(data, dict) else None
    return [
        {"name": str(j.get("name", "?")), "labels": list(j.get("labels") or [])}
        for j in jobs or []
        if isinstance(j, dict) and _lower(j.get("status")) in QUEUED_STATUSES
    ]


QUEUE_WARN_SEC = 600  # 10 min queued = surface a warning (#440)
STEP_WARN_SEC = 300  # 5 min on the same step = possible stuck (#440)


def _now_epoch_ms() -> int:
    return int(time.time() * 1000)


def _parse_iso(ts: str | None) -> float | None:
    """Parse an RFC3339 / ISO-8601 timestamp into epoch seconds. Returns
    None on any malformed input."""
    if not ts:
        return None
    try:
        # Strip trailing Z; datetime.fromisoformat handles offsets but
        # not the 'Z' suffix until Python 3.11.
        ts = ts.replace("Z", "+00:00")
        from datetime import datetime

        return datetime.fromisoformat(ts).timestamp()
    except (ValueError, TypeError):
        return None


def fetch_run_jobs(run_id: str, repo: str | None) -> dict | None:
    """Call gh run view --json jobs,status,conclusion,createdAt,updatedAt
    for the given run; return the parsed dict or None on error."""
    args = [
        "run",
        "view",
        str(run_id),
        "--json",
        "jobs,status,conclusion,createdAt,updatedAt,workflowName",
    ]
    if repo:
        args.extend(["--repo", repo])
    res = gh(*args)
    if not res.ok:
        return None
    try:
        return json.loads(res.stdout)
    except json.JSONDecodeError:
        return None


def aggregate_jobs(run_info: dict) -> dict:
    """Compute per-poll aggregate stats + per-job rows from a
    `gh run view --json jobs` payload."""
    jobs = run_info.get("jobs", []) or []
    counts = {"total": len(jobs), "queued": 0, "in_progress": 0, "completed": 0, "failed": 0}
    current: list[dict] = []
    warnings: list[str] = []
    now = time.time()
    for j in jobs:
        status = j.get("status") or ""
        conclusion = j.get("conclusion") or ""
        if status == "queued":
            counts["queued"] += 1
            started = _parse_iso(j.get("startedAt") or j.get("createdAt"))
            if started and (now - started) > QUEUE_WARN_SEC:
                warnings.append(
                    f"{j.get('name', '?')} has been queued for "
                    f"{int(now - started)}s (> {QUEUE_WARN_SEC}s threshold)"
                )
        elif status == "in_progress":
            counts["in_progress"] += 1
            current.append(_summarize_job(j, now, warnings))
        elif status == "completed":
            counts["completed"] += 1
            if conclusion in {"failure", "cancelled", "timed_out"}:
                counts["failed"] += 1
    total = counts["total"] or 1  # avoid div-by-zero
    percent = round((counts["completed"] / total) * 100)
    return {
        "counts": counts,
        "percent_complete": percent,
        "current_jobs": current,
        "warnings": warnings,
    }


def _summarize_job(j: dict, now: float, warnings: list[str]) -> dict:
    name = j.get("name", "?")
    started = _parse_iso(j.get("startedAt"))
    elapsed_sec = int(now - started) if started else None
    current_step = None
    for step in j.get("steps", []) or []:
        if step.get("status") == "in_progress":
            step_started = _parse_iso(step.get("startedAt"))
            step_elapsed = int(now - step_started) if step_started else None
            current_step = {
                "name": step.get("name", "?"),
                "number": step.get("number"),
                "started_at": step.get("startedAt"),
                "elapsed_sec": step_elapsed,
            }
            if step_elapsed and step_elapsed > STEP_WARN_SEC:
                warnings.append(
                    f"{name}: step '{step.get('name', '?')}' has held "
                    f"for {step_elapsed}s (> {STEP_WARN_SEC}s threshold)"
                )
            break
    return {
        "name": name,
        "started_at": j.get("startedAt"),
        "elapsed_sec": elapsed_sec,
        "current_step": current_step,
    }


def emit_progress_report(
    pr: int, repo: str | None, snapshot: PRSnapshot, checks: list[CheckRow]
) -> None:
    """Emit a per-poll progress report covering active workflow runs
    associated with the PR's head SHA.

    JSONL line on stdout (schema-versioned), human-readable per-job
    rows on stderr."""
    seen_runs: set[str] = set()
    for c in checks:
        if c.bucket != "pending":
            continue
        run_id = _extract_run_id_from_link(c.link)
        if not run_id or run_id in seen_runs:
            continue
        seen_runs.add(run_id)
        info = fetch_run_jobs(run_id, repo)
        if not info:
            continue
        agg = aggregate_jobs(info)
        report = {
            "v": 1,
            "ts_ms": _now_epoch_ms(),
            "pr": pr,
            "run_id": run_id,
            "sha": snapshot.head_sha[:7] if snapshot.head_sha else None,
            "workflow": info.get("workflowName"),
            "status": info.get("status"),
            "conclusion": info.get("conclusion") or None,
            "jobs": agg["counts"],
            "percent_complete": agg["percent_complete"],
            "current_jobs": agg["current_jobs"],
            "warnings": agg["warnings"],
        }
        print(json.dumps(report))
        # Human-readable rows on stderr so stdout stays machine-parseable.
        c1 = agg["counts"]
        print(
            f"  {info.get('workflowName', '?')} run {run_id} "
            f"sha={(snapshot.head_sha or '?')[:7]} "
            f"status={info.get('status')} "
            f"{c1['completed']}/{c1['total']} jobs done "
            f"({agg['percent_complete']}%)",
            file=sys.stderr,
        )
        for j in agg["current_jobs"]:
            step = j.get("current_step") or {}
            step_str = step.get("name", "—") if step else "—"
            print(
                f"    {j['name']:<24} in_progress  "
                f"{step_str:<22} "
                f"{j.get('elapsed_sec', '—')}s "
                f"step elapsed {step.get('elapsed_sec', '—')}s",
                file=sys.stderr,
            )
        for w in agg["warnings"]:
            print(f"    WARN: {w}", file=sys.stderr)


def _sleep_remaining_interval(poll_started: float, interval: int) -> None:
    remaining = max(0.0, interval - (time.monotonic() - poll_started))
    if remaining:
        time.sleep(remaining)


def watch(
    pr: int,
    repo: str | None,
    interval: int,
    timeout: int,
    require_pattern: str | None,
    opts: CancelOptions,
    log: WatchLog | None = None,
    *,
    no_checks_grace: int = DEFAULT_NO_CHECKS_GRACE_SEC,
    max_queued: int | None = None,
) -> int:
    deadline = time.monotonic() + timeout
    snapshot: PRSnapshot | None = None
    required_names: set[str] | None = None
    # Initial snapshot — bail fast if the PR isn't open. A failed fetch is
    # GitHub being unreachable, never "closed" (#1418).
    for attempt in range(1, MAX_CONSECUTIVE_API_FAILURES + 1):
        snapshot = PRSnapshot.fetch(pr, repo)
        if snapshot is not None:
            break
        print(f"ERROR  could not fetch PR #{pr} (attempt {attempt})", file=sys.stderr)
        if log:
            log.emit("api_degraded", source="pr_snapshot", reason="fetch_failed")
        if attempt < MAX_CONSECUTIVE_API_FAILURES:
            time.sleep(interval)
    if snapshot is None:
        _exit_unreachable(f"could not fetch PR #{pr}", log)
        return EXIT_GITHUB_UNREACHABLE
    if snapshot.state in {"MERGED", "CLOSED"}:
        print(f"PR-STATE  #{pr} state={snapshot.state}")
        if log:
            log.emit("pr_state", state=snapshot.state, head_sha=snapshot.head_sha)
        code = EXIT_GREEN if snapshot.state == "MERGED" else EXIT_PR_CLOSED
        label = "merged" if snapshot.state == "MERGED" else "closed"
        _exit_after_cancel(code, label, pr, repo, snapshot.head_sha, opts, log)

    # Resolve required check names (best-effort). Without a repository every
    # poll would fail, so that is fatal up front (#1418).
    repo_for_protection = repo or _resolve_origin_repo()
    if not repo_for_protection:
        _exit_unreachable("no repository: pass --repo or add an `origin` remote", log)
        return EXIT_GITHUB_UNREACHABLE
    required_names = fetch_required_check_names(repo_for_protection, snapshot.base_ref)
    opts = replace(opts, pinned_sha=snapshot.head_sha)
    seen_head = snapshot.head_sha
    # Empty-rollup bookkeeping (#1418), reset whenever the head moves.
    idle_since: float | None = None
    no_checks_reasons: dict[str, str | None] = {}
    api_failures = 0
    unknown_polls = 0

    review_state = ReviewState()
    coderabbit_probe_complete = False

    require_re = re.compile(require_pattern) if require_pattern else None

    while True:
        if time.monotonic() >= deadline:
            print(f"TIMEOUT  after {timeout}s")
            if log:
                log.emit("timeout", timeout_sec=timeout)
            _exit_after_cancel(EXIT_TIMEOUT, "timeout", pr, repo, snapshot.head_sha, opts, log)
        poll_started = time.monotonic()

        # One current GraphQL snapshot owns PR state, check rollup, human
        # reviews, and optional CodeRabbit fields. This prevents bot APIs from
        # becoming a serial post-green gate.
        gate = fetch_gate_snapshot(
            repo_for_protection,
            pr,
            include_coderabbit=not coderabbit_probe_complete or review_state.coderabbit_enabled,
        )
        if gate is None:
            api_failures += 1
            print("NOTE  gate snapshot unavailable; retrying", file=sys.stderr, flush=True)
            if log:
                log.emit(
                    "api_degraded",
                    source="gate_snapshot",
                    reason="fetch_failed",
                    consecutive=api_failures,
                )
            if api_failures >= MAX_CONSECUTIVE_API_FAILURES:
                _exit_unreachable(f"{api_failures} consecutive failed polls", log)
            _sleep_remaining_interval(poll_started, interval)
            continue
        api_failures = 0
        snapshot = gate.pr
        if snapshot.head_sha != seen_head:
            print(
                f"NOTE  head moved {seen_head[:7]} -> {snapshot.head_sha[:7]}",
                file=sys.stderr,
            )
            if log:
                log.emit("head_moved", old=seen_head, new=snapshot.head_sha)
            seen_head = snapshot.head_sha
            idle_since = None
            unknown_polls = 0
        merge_fields = {"mergeable": snapshot.mergeable, "merge_state_status": snapshot.merge_state}
        if snapshot.state != "OPEN":
            print(f"PR-STATE  #{pr} state={snapshot.state}")
            if log:
                log.emit("pr_state", state=snapshot.state, head_sha=snapshot.head_sha)
            code = EXIT_GREEN if snapshot.state == "MERGED" else EXIT_PR_CLOSED
            label = "merged" if snapshot.state == "MERGED" else "closed"
            _exit_after_cancel(code, label, pr, repo, snapshot.head_sha, opts, log)

        # 1. Judge the head commit's checks. The REST check runs (every page)
        # are judged by the supersession rule; the rollup rows are the
        # fallback when that data is unavailable.
        verdict: Verdict | None = None
        checks = gate.checks
        # Idle: nothing has registered on the head. Idle for the grace period
        # means nothing will.
        # Anything in the rollup (an external CI's status, a check app) means
        # checks do report here, so only a fully empty head is idle.
        idle = not gate.checks
        if gate.head_checks is not None:
            idle = idle and not gate.head_checks.statuses and not any(
                isinstance(r, dict) and r.get("head_sha") == snapshot.head_sha
                for r in gate.head_checks.workflow_runs
            )
        if not idle:
            idle_since = None
        elif idle_since is None:
            idle_since = poll_started
        grace_elapsed = idle_since is not None and poll_started - idle_since >= no_checks_grace
        if gate.head_checks is not None:
            if max_queued is not None:
                _exit_if_queued_too_long(gate.head_checks, snapshot, repo_for_protection,
                                         max_queued, log)
            verdict = judge_check_runs(
                gate.head_checks.check_runs,
                gate.head_checks.workflow_runs,
                snapshot.head_sha,
                required_names,
                statuses=gate.head_checks.statuses,
                head_branch=snapshot.head_ref or None,
                require_re=require_re,
                runs_grace_elapsed=grace_elapsed,
            )
            checks = [j.as_check_row() for j in verdict.judgments]
        pending = [c for c in checks if c.bucket == "pending"]
        failing = [c for c in checks if c.bucket in {"fail", "cancel"}]
        counts = check_counts(checks)
        if counts["total"] == 0 and (verdict is None or verdict.state == "pending"):
            # A fresh push registers no checks for the first few seconds; an
            # empty rollup must read as "no data yet", never as green.
            if log:
                log.emit("checks", checks=counts, note="rollup_empty", **merge_fields)
            # ...unless nothing will ever register (#1418).
            if snapshot.head_sha not in no_checks_reasons:
                no_checks_reasons[snapshot.head_sha] = immediate_no_checks_reason(
                    repo_for_protection, snapshot
                )
            if snapshot.mergeable == "CONFLICTING" or snapshot.merge_state == "DIRTY":
                _exit_conflict(pr, snapshot, log, merge_fields)
            immediate = no_checks_reasons[snapshot.head_sha]
            idle_for = poll_started - idle_since if idle_since is not None else 0.0
            if immediate and idle_for < NO_CHECKS_SETTLE_SEC:
                time.sleep(NO_CHECKS_SETTLE_SEC - idle_for)
                continue
            reason = immediate or ("empty_after_grace" if grace_elapsed else None)
            if reason:
                _exit_no_checks(reason, snapshot, required_names, require_re, log)
            _sleep_remaining_interval(poll_started, interval)
            continue
        if log:
            log.emit("checks", checks=counts, **merge_fields)
            elapsed = round(max(0.0, time.monotonic() - log.started_monotonic), 2)
            print(
                f"{elapsed:.2f} {counts['succeeded']} succeeded, "
                f"{counts['failed']} failed, {counts['pending']} pending",
                file=sys.stderr,
                flush=True,
            )

        if verdict is not None:
            if _act_on_verdict(verdict, pr, repo, repo_for_protection, snapshot, opts, log):
                # The head moved under a cancellation-derived verdict: drop
                # it and judge the new head on the next poll.
                _sleep_remaining_interval(poll_started, interval)
                continue
            failing = [j.as_check_row() for j in verdict.advisory_failing]
        else:
            # Classify each failing check as required or advisory.
            for c in failing:
                if not _is_required(c, required_names, require_re):
                    continue
                # First failing required check → bail.
                if log:
                    log.emit(
                        "required_failure",
                        check={
                            "name": c.name,
                            "state": c.state or c.bucket,
                            "link": c.link,
                        },
                    )
                # Diagnose first, then cancel. The probe is one bounded request
                # against the failing *job*, so the matrix minutes this costs are
                # seconds; cancelling first raced the log's availability and left
                # the caller with a bare "FAIL <name>" and nothing to act on,
                # which is the opposite of what failing fast is for.
                report = _build_failure_report(c, repo_for_protection)
                _cancel_for_exit("fail", pr, repo, snapshot.head_sha, opts, log)
                if "fail" in opts.on or "always" in opts.on:
                    print(
                        f"NOTE  {len(pending)} check(s) still running on this head SHA; "
                        "cancelling this PR's remaining runs — push a fix to supersede them"
                    )
                print(report.render())
                if log and (report.first_error or report.classifier):
                    log.emit(
                        "failure_diagnostic",
                        check_name=c.name,
                        first_error=report.first_error,
                        classifier=report.classifier,
                    )
                _finish_exit(EXIT_REQUIRED_FAIL, "fail", log)

        # A conflict never resolves by waiting (#1418).
        if snapshot.mergeable == "CONFLICTING" or snapshot.merge_state == "DIRTY":
            _exit_conflict(pr, snapshot, log, merge_fields)

        if not coderabbit_probe_complete:
            probe = gate.coderabbit_probe or CodeRabbitProbe("degraded", 0)
            if (
                probe.state != "detected"
                and gate.coderabbit is not None
                and (gate.coderabbit.actionable or gate.coderabbit.state == "skipped")
            ):
                # Current-PR bot output is direct presence evidence even if
                # the historical five-PR sample degraded.
                probe = CodeRabbitProbe("detected", probe.sampled_merged_prs)
            if log:
                log.emit(
                    "coderabbit",
                    coderabbit={
                        "state": probe.state,
                        "sampled_merged_prs": probe.sampled_merged_prs,
                    },
                )
            if probe.state != "degraded":
                coderabbit_probe_complete = True
                review_state.coderabbit_enabled = probe.state == "detected"

        observation = gate.coderabbit if review_state.coderabbit_enabled else None
        if review_state.update_prefetched(gate.human_review_ids, observation, log):
            print("REVIEW  new review activity")
            if log:
                log.emit("review_activity", state="actionable")
            _exit_after_cancel(
                EXIT_REVIEW_ACTIVITY, "review", pr, repo, snapshot.head_sha, opts, log
            )

        # CI and review state came from the same GraphQL response, so green
        # does not initiate or wait for an additional CodeRabbit request.
        checks_green = verdict.state == "pass" if verdict is not None else not pending
        unknown_polls = unknown_polls + 1 if checks_green and snapshot.mergeable == "UNKNOWN" else 0
        if unknown_polls >= MERGEABLE_UNKNOWN_MAX_POLLS:
            print(
                f"CONFLICT  #{pr} checks green but mergeable stayed UNKNOWN for "
                f"{unknown_polls} polls (mergeStateStatus={snapshot.merge_state})"
            )
            _finish_exit(EXIT_CONFLICT, "mergeable_unknown", log, **merge_fields)
        if checks_green and snapshot.mergeable == "MERGEABLE":
            for c in failing:
                print(f"ADVISORY-FAIL  {c.name} (not in required set)")
            print(f"GREEN  #{pr} all required checks passed")
            if log:
                log.emit("green", mergeable=snapshot.mergeable, checks=counts)
            _exit_after_cancel(
                EXIT_GREEN,
                "always" if "always" in opts.on else "never",
                pr,
                repo,
                snapshot.head_sha,
                opts,
                log,
            )

        try:
            emit_progress_report(pr, repo, snapshot, checks)
        except Exception as exc:
            print(f"NOTE  progress report failed: {exc}", file=sys.stderr)

        _sleep_remaining_interval(poll_started, interval)


def _exit_conflict(
    pr: int, snapshot: PRSnapshot, log: WatchLog | None, merge_fields: dict[str, str]
) -> None:
    print(
        f"CONFLICT  #{pr} mergeable={snapshot.mergeable} "
        f"mergeStateStatus={snapshot.merge_state}: rebase or merge the base branch"
    )
    _finish_exit(EXIT_CONFLICT, "conflict", log, **merge_fields)


def _exit_no_checks(
    reason: str,
    snapshot: PRSnapshot,
    required: set[str] | None,
    require_re: re.Pattern[str] | None,
    log: WatchLog | None,
) -> None:
    """Terminal empty rollup: a required check can then never report (exit
    6); otherwise nothing gates the merge but `mergeStateStatus` (exit 8)."""
    if required and require_re is None:
        missing = sorted(required)
        print(f"NEVER-REPORTED  {', '.join(missing)}: required, but no check runs ({reason})")
        if log:
            log.emit("never_reported", checks=missing, reason=reason)
        _finish_exit(EXIT_NEVER_REPORTED, "never_reported", log)
    print(
        f"NO-CHECKS  #{snapshot.number} reason={reason} "
        f"mergeStateStatus={snapshot.merge_state}"
    )
    if log:
        log.emit(
            "no_checks",
            reason=reason,
            merge_state_status=snapshot.merge_state,
            mergeable=snapshot.mergeable,
            head_sha=snapshot.head_sha,
        )
    _finish_exit(EXIT_NO_CHECKS, "no_checks", log, merge_state_status=snapshot.merge_state)


def _exit_if_queued_too_long(
    head: HeadChecks,
    snapshot: PRSnapshot,
    repo: str,
    max_queued: int,
    log: WatchLog | None,
) -> None:
    now = time.time()
    stuck = [
        run
        for run in head.workflow_runs
        if isinstance(run, dict)
        and run.get("head_sha") == snapshot.head_sha
        and _lower(run.get("status")) in QUEUED_STATUSES
        and (created := _parse_iso(run.get("created_at"))) is not None
        and now - created > max_queued
    ]
    if not stuck:
        return
    jobs: list[dict] = []
    for run in stuck:
        run_id = _as_int(run.get("id"))
        if run_id is None:
            continue
        found = fetch_queued_jobs(repo, run_id)
        if not found:
            found = [{"name": str(run.get("name") or run_id), "labels": []}]
        jobs += [{**job, "run_id": run_id} for job in found]
    for job in jobs:
        labels = ", ".join(job["labels"]) or "?"
        print(f"QUEUED  {job['name']} (run {job['run_id']}) waiting for a runner: {labels}")
    if log:
        log.emit("queued_too_long", max_queued_sec=max_queued, jobs=jobs)
    _finish_exit(EXIT_QUEUED, "queued", log)


def _act_on_verdict(
    verdict: Verdict,
    pr: int,
    repo: str | None,
    repo_for_reports: str | None,
    snapshot: PRSnapshot,
    opts: CancelOptions,
    log: WatchLog | None = None,
) -> bool:
    """Exit on a terminal verdict. Returns True when the verdict was dropped
    because the PR's head moved; False when it is pending or passing."""
    for note in verdict.notes:
        print(f"NOTE  {note}", file=sys.stderr)
        if log:
            log.emit("check_note", note=note)
    if verdict.state == "approval_required":
        names = [j.name for j in verdict.judgments if j.state == "approval_required"]
        print(f"APPROVAL-REQUIRED  {', '.join(names)}: a maintainer must approve the run(s)")
        if log:
            log.emit("approval_required", checks=names)
        _finish_exit(EXIT_APPROVAL_REQUIRED, "approval_required", log)
    if verdict.state == "never_reported":
        print(
            f"NEVER-REPORTED  {', '.join(verdict.missing)}: required but no check run exists "
            "and every workflow run on this head commit has finished"
        )
        if log:
            log.emit("never_reported", checks=verdict.missing)
        _finish_exit(EXIT_NEVER_REPORTED, "never_reported", log)
    if verdict.state == "stale":
        names = [j.name for j in verdict.judgments if j.required and j.state == "stale"]
        print(f"STALE  {', '.join(names)}: GitHub marked the check stale; re-run needed")
        if log:
            log.emit("stale", checks=names)
        _finish_exit(EXIT_STALE, "stale", log)
    if verdict.state != "fail":
        return False

    if verdict.cancellation_derived:
        # A cancellation is often concurrency reacting to a new push. Never
        # exit on stale data: confirm the head before acting.
        fresh = PRSnapshot.fetch(pr, repo)
        if fresh is None or fresh.head_sha != snapshot.head_sha:
            moved_to = fresh.head_sha if fresh is not None else None
            print(
                f"NOTE  head moved {snapshot.head_sha[:7]} -> {(moved_to or '?')[:7]}; "
                "dropping the verdict",
                file=sys.stderr,
            )
            if log:
                log.emit("head_moved", old=snapshot.head_sha, new=moved_to)
            return True

    first = verdict.failing[0]
    if log:
        log.emit(
            "required_failure",
            check={
                "name": first.name,
                "state": first.conclusion,
                "link": first.link,
                "workflow": first.workflow,
            },
        )
    # Diagnose first, then cancel (see the rollup path for why).
    reports = [_build_failure_report(first.as_check_row(), repo_for_reports)]
    reports += [
        FailureReport(j.as_check_row(), str(j.run_id) if j.run_id else None, "", None)
        for j in verdict.failing[1:]
    ]
    _cancel_for_exit(
        "fail", pr, repo, snapshot.head_sha, opts, log, scope=dict(verdict.failing_run_ids)
    )
    if "fail" in opts.on or "always" in opts.on:
        print(
            "NOTE  cancelling the failing workflow's runs at or below the failing run; "
            "newer runs and other workflows are left alone"
        )
    for judgment, report in zip(verdict.failing, reports):
        print(report.render())
        if judgment.workflow_broken:
            print("  note:       startup_failure: the workflow is broken, not the code")
    if log and (reports[0].first_error or reports[0].classifier):
        log.emit(
            "failure_diagnostic",
            check_name=first.name,
            first_error=reports[0].first_error,
            classifier=reports[0].classifier,
        )
    _finish_exit(EXIT_REQUIRED_FAIL, "fail", log)
    return False


def _is_required(
    c: CheckRow, required: set[str] | None, require_re: re.Pattern[str] | None
) -> bool:
    if require_re is not None:
        return bool(require_re.search(c.name))
    if not required:
        # No protection (None), no allowlist, OR protection that lists zero
        # checks (empty set): treat every check as required so a red lane
        # fails the wait immediately instead of the watcher idling until
        # the whole matrix — including the slow Mac lanes — finishes.
        return True
    return c.name in required


def _build_failure_report(c: CheckRow, repo: str | None) -> FailureReport:
    run_id = _extract_run_id_from_link(c.link)
    # A failing check's link is `…/actions/runs/<run>/job/<job>`. `job_id` was
    # declared for this and never assigned, so every probe fell back to the
    # run-level log — the one that is unavailable while the run is in
    # progress, which on this path it always is.
    job_id = c.job_id or _extract_job_id_from_link(c.link)
    first_err, classifier = ("", None)
    if repo and (run_id or job_id):
        first_err, classifier = classify_failure(repo, run_id, job_id)
    return FailureReport(check=c, run_id=run_id, first_error=first_err, classifier=classifier)


def _extract_run_id_from_link(link: str | None) -> str | None:
    if not link:
        return None
    m = re.search(r"/actions/runs/(\d+)", link)
    return m.group(1) if m else None


def _extract_job_id_from_link(link: str | None) -> str | None:
    if not link:
        return None
    m = re.search(r"/job/(\d+)", link)
    return m.group(1) if m else None


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name, "").strip()
    try:
        return int(raw) if raw else default
    except ValueError:
        return default


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="pr_merge_watch",
        description="Fail-fast PR-check waiter for clud (issue #408).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "exit codes: 0 green, 1 required check failed, 2 review activity, "
            "3 PR closed, 4 timeout, 5 approval required, 6 required check never "
            "reported, 7 stale (re-run needed), 8 no checks will ever report, "
            "9 merge conflict, 10 GitHub unreachable, 11 queued too long, "
            "130/143 killed\n\n"
            "supersession rule (#1330): checks on the PR's current head commit are "
            "grouped by (workflow file, check name) and ordered by check-run id. A "
            "cancelled check is replaced by any newer check, even a queued one; a "
            "completed result only by a newer check that actually ran (not skipped). "
            "A cancelled check with no newer check and no newer run of its workflow "
            "fails. success, neutral and skipped pass. A cancellation-derived failure "
            "is acted on only after re-reading the head; on failure only the failing "
            "workflow's runs at or below the failing run are cancelled."
        ),
    )
    p.add_argument("pr_number", type=int, help="PR number to watch")
    p.add_argument("--repo", help="owner/name (defaults to current repo's origin)")
    p.add_argument(
        "--interval",
        type=int,
        default=20,
        help="seconds between polls (default 20)",
    )
    p.add_argument(
        "--timeout",
        type=int,
        default=_env_int("CLUD_PR_MERGE_WATCH_TIMEOUT", DEFAULT_TIMEOUT_SEC),
        help="overall wait cap in seconds (default $CLUD_PR_MERGE_WATCH_TIMEOUT, else "
        "3600); keep it below any tool-call cap of the caller",
    )
    p.add_argument(
        "--no-checks-grace",
        type=int,
        default=_env_int("CLUD_PR_MERGE_WATCH_NO_CHECKS_GRACE", DEFAULT_NO_CHECKS_GRACE_SEC),
        help="seconds an empty rollup is waited for before exiting 8 NO_CHECKS "
        "(default $CLUD_PR_MERGE_WATCH_NO_CHECKS_GRACE, else 60)",
    )
    p.add_argument(
        "--max-queued",
        type=int,
        default=None,
        help="exit 11 when a run has waited this many seconds for a runner (default: off)",
    )
    p.add_argument(
        "--require",
        default=None,
        help="regex of check names to treat as required when branch "
        "protection is absent or inaccessible",
    )
    # Cancellation control
    p.add_argument(
        "--cancel-on",
        default=",".join(sorted(CANCEL_ON_DEFAULTS)),
        help="comma-separated subset of "
        f"{sorted(CANCEL_ON_CHOICES)} (default: fail,review,timeout,closed)",
    )
    p.add_argument(
        "--cancel-mode",
        default="runs",
        choices=sorted(CANCEL_MODE_CHOICES),
        help="granularity of cancellation (default: runs)",
    )
    p.add_argument(
        "--cancel-timeout",
        type=int,
        default=30,
        help="seconds to wait for cancellations to settle (default 30)",
    )
    p.add_argument("--no-cancel", action="store_true", help="shortcut for --cancel-on=never")
    p.add_argument(
        "--require-cancel",
        action="store_true",
        help="mark cancellation API errors as required in the event log",
    )
    p.add_argument(
        "--dry-run-cancel",
        action="store_true",
        help="list workflow runs that would be cancelled without POSTing",
    )
    p.add_argument(
        "--ignore-permission-errors",
        dest="ignore_perm",
        action="store_true",
        default=True,
        help="warn + continue on cancel 403s (default; --no-ignore-permission-errors flips)",
    )
    p.add_argument("--no-ignore-permission-errors", dest="ignore_perm", action="store_false")
    p.add_argument(
        "--no-retry", action="store_true", help="disable backoff/retry on cancel API calls"
    )
    return p.parse_args(argv)


def _resolve_cancel_options(ns: argparse.Namespace) -> CancelOptions:
    raw = "never" if ns.no_cancel else ns.cancel_on
    parts = {p.strip() for p in raw.split(",") if p.strip()}
    invalid = parts - CANCEL_ON_CHOICES
    if invalid:
        raise SystemExit(
            f"--cancel-on: invalid values {sorted(invalid)}; "
            f"choices are {sorted(CANCEL_ON_CHOICES)}"
        )
    if "never" in parts:
        parts = set()
    elif "always" in parts:
        parts = CANCEL_ON_CHOICES - {"never", "always"}
        parts.add("always")
    return CancelOptions(
        on=parts,
        mode="none" if ns.no_cancel else ns.cancel_mode,
        timeout=ns.cancel_timeout,
        require=ns.require_cancel,
        dry_run=ns.dry_run_cancel,
        ignore_permission_errors=ns.ignore_perm,
        no_retry=ns.no_retry,
    )


def main(argv: list[str] | None = None) -> int:
    ns = parse_args(argv if argv is not None else sys.argv[1:])
    opts = _resolve_cancel_options(ns)
    log = WatchLog.create(ns.pr_number, ns.repo)
    # Honor an env override for testing.
    if os.environ.get("CLUD_PR_MERGE_WATCH_DRY_RUN") == "1":
        print(
            f"DRY-RUN pr={ns.pr_number} repo={ns.repo or 'origin'} "
            f"timeout={ns.timeout} "
            f"require={ns.require or 'branch-protection'} cancel_on={sorted(opts.on)}"
        )
        log.emit("dry_run", timeout=ns.timeout, cancel_on=sorted(opts.on))
        log.emit("EXIT", code=EXIT_GREEN, reason="dry_run")
        log.close()
        return EXIT_GREEN
    install_kill_handlers()
    try:
        code = watch(
            ns.pr_number,
            ns.repo,
            ns.interval,
            ns.timeout,
            ns.require,
            opts,
            log,
            no_checks_grace=ns.no_checks_grace,
            max_queued=ns.max_queued,
        )
    except WatchKilled as killed:
        print(f"KILLED  {killed}", file=sys.stderr)
        _finish_exit(killed.code, "killed", log, signal=killed.signum)
        return killed.code
    if not log.closed:
        log.emit("EXIT", code=code, reason="return")
        log.close()
    return code


if __name__ == "__main__":
    sys.exit(main())
