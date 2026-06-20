#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""pr_merge_watch.py — fail-fast PR-check waiter for clud.

Polls a GitHub PR's checks; returns the moment a required check fails,
new review activity arrives, the PR closes/merges, or the timeout fires.
On non-success exits (and defensively on success), cancels still-running
workflow runs on the PR's head SHA so we stop burning matrix minutes on
results we've already decided to ignore.

Invoked via `clud tool run github/pr_merge_watch.py …` so UV_CACHE_DIR
is pinned to ~/.clud/cache/uv per the three-layer enforcement (see
issue #408).

Exit codes:
  0  all required checks green AND mergeable=MERGEABLE
  1  at least one required check failed (details on stdout)
  2  new review activity (unresolved coderabbit/human review)
  3  PR closed or merged out from under us
  4  timeout (configurable via --timeout, default 60min)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field

EXIT_GREEN = 0
EXIT_REQUIRED_FAIL = 1
EXIT_REVIEW_ACTIVITY = 2
EXIT_PR_CLOSED = 3
EXIT_TIMEOUT = 4

CANCEL_ON_CHOICES = {"fail", "review", "timeout", "closed", "always", "never"}
CANCEL_ON_DEFAULTS = {"fail", "review", "timeout", "closed"}
CANCEL_MODE_CHOICES = {"runs", "jobs", "none"}

# First-error classifier patterns. Applied to up to ~200 lines of
# `gh run view --log-failed`. Order matters: first match wins.
CLASSIFIERS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"^Diff in .*?:\d+:|run `cargo fmt", re.MULTILINE), "rustfmt drift"),
    (re.compile(r"^error: .*?clippy::|warning:.*?clippy::", re.MULTILINE), "clippy warning"),
    (re.compile(r"^error\[E\d+\]:|^error: could not compile", re.MULTILINE), "compile error"),
    (re.compile(r"^thread .*? panicked at|FAILED \(\d+\)|test result: FAILED", re.MULTILINE),
     "test failure"),
    (re.compile(r"^ruff (check|format).*?(failed|error)", re.MULTILINE | re.IGNORECASE),
     "ruff violation"),
    (re.compile(r"timed out|connection refused|temporary failure|EAI_AGAIN|503 Service",
                re.MULTILINE | re.IGNORECASE), "network/transient"),
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


def gh(*args: str, check: bool = False) -> GhResult:
    """Run gh with the supplied args; return the captured outcome."""
    res = subprocess.run(["gh", *args], capture_output=True, text=True)  # noqa: S603, S607
    if check and res.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {res.stderr.strip()}")
    return GhResult(res.returncode, res.stdout, res.stderr)


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

    @classmethod
    def fetch(cls, pr: int, repo: str | None) -> "PRSnapshot | None":
        args = ["pr", "view", str(pr)]
        if repo:
            args += ["--repo", repo]
        args += ["--json", "number,state,mergeable,headRefOid,baseRefName"]
        data = gh_json(*args)
        if not isinstance(data, dict):
            return None
        return cls(
            number=int(data.get("number", pr)),
            state=str(data.get("state", "UNKNOWN")),
            mergeable=str(data.get("mergeable", "UNKNOWN")),
            head_sha=str(data.get("headRefOid", "")),
            base_ref=str(data.get("baseRefName", "main")),
        )


@dataclass
class CheckRow:
    name: str
    bucket: str  # pass | fail | pending | skipping
    state: str
    link: str | None = None
    job_id: str | None = None  # populated lazily for failing checks


def fetch_checks(pr: int, repo: str | None) -> list[CheckRow]:
    args = ["pr", "checks", str(pr), "--json", "name,bucket,state,link"]
    if repo:
        args = ["pr", "checks", str(pr), "--repo", repo,
                "--json", "name,bucket,state,link"]
    # `gh pr checks` returns exit 1 when ANY check failed even with --json;
    # capture stdout regardless.
    res = gh(*args)
    if not res.stdout.strip():
        return []
    try:
        rows = json.loads(res.stdout)
    except json.JSONDecodeError:
        return []
    if not isinstance(rows, list):
        return []
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
    data = gh_json("api",
                   f"repos/{repo}/branches/{base_ref}/protection/required_status_checks")
    if not isinstance(data, dict):
        return None
    contexts = data.get("contexts")
    if isinstance(contexts, list):
        return {str(c) for c in contexts}
    return set()


def classify_failure(repo: str, run_id: str, job_id: str | None) -> tuple[str, str | None]:
    """Fetch `gh run view --log-failed` for the run/job; classify the first error.

    Returns (first_error_line, classifier_label).
    """
    args = ["run", "view", run_id, "--repo", repo, "--log-failed"]
    if job_id:
        args = ["run", "view", run_id, "--repo", repo,
                "--job", job_id, "--log-failed"]
    res = gh(*args)
    text = res.stdout if res.ok else ""
    # Take a manageable slice — most CI failure logs surface the root in
    # the first few hundred lines.
    sample = "\n".join(text.splitlines()[:300])
    first_err = ""
    for line in sample.splitlines():
        if re.search(r"^error|^Error|^FAILED|panicked|Diff in", line):
            first_err = line.strip()
            break
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

@dataclass
class ReviewState:
    """Tracks what review activity we've already observed so we only
    report NEW activity. Stateful across a single waiter run."""
    seen_review_ids: set[int] = field(default_factory=set)
    initialized: bool = False

    def update(self, pr: int, repo: str | None) -> bool:
        """Return True if NEW reviews / threads have arrived since last call."""
        args = ["api", f"repos/{{owner}}/{{repo}}/pulls/{pr}/reviews"] if not repo \
            else ["api", f"repos/{repo}/pulls/{pr}/reviews"]
        # gh resolves {owner}/{repo} from the current cwd's remote when no
        # --repo given; substitute manually for the no-repo case.
        if not repo:
            origin = gh("api", f"repos/{{owner}}/{{repo}}/pulls/{pr}/reviews")
            text = origin.stdout if origin.ok else ""
        else:
            text = gh(*args).stdout
        try:
            reviews = json.loads(text) if text.strip() else []
        except json.JSONDecodeError:
            reviews = []
        if not isinstance(reviews, list):
            return False
        # Identify "actionable" reviews: state in {CHANGES_REQUESTED, COMMENTED}
        # from non-bot accounts, OR coderabbit reviews of any state.
        new_ids: set[int] = set()
        for r in reviews:
            if not isinstance(r, dict):
                continue
            rid = r.get("id")
            if not isinstance(rid, int):
                continue
            user = (r.get("user") or {}).get("login", "")
            state = r.get("state", "")
            actionable = (state in {"CHANGES_REQUESTED", "COMMENTED"}
                          and "[bot]" not in user) or user == "coderabbitai[bot]"
            if actionable:
                new_ids.add(rid)
        if not self.initialized:
            self.seen_review_ids = new_ids
            self.initialized = True
            return False
        diff = new_ids - self.seen_review_ids
        self.seen_review_ids = new_ids
        return bool(diff)


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


def cancel_pr_runs(pr: int, repo: str | None, head_sha: str, opts: CancelOptions) -> int:
    """Cancel non-completed workflow runs on the PR's head SHA.

    Returns the number of cancel attempts. Failures are surfaced as
    `CANCEL <id> status=…` lines on stdout.
    """
    if opts.mode == "none":
        return 0
    repo_arg = repo if repo else _resolve_origin_repo()
    if not repo_arg:
        print(f"CANCEL  skipped: could not resolve repo for PR #{pr}")
        return 0
    runs_resp = gh_json("api",
                       f"repos/{repo_arg}/actions/runs?head_sha={head_sha}&per_page=100")
    if not isinstance(runs_resp, dict):
        return 0
    runs = runs_resp.get("workflow_runs", [])
    if not isinstance(runs, list):
        return 0
    attempts = 0
    for r in runs:
        if not isinstance(r, dict):
            continue
        rid = r.get("id")
        status = r.get("status", "")
        if status in {"completed", "cancelled"} or not isinstance(rid, int):
            continue
        attempts += 1
        if opts.dry_run:
            print(f"CANCEL  workflow_run={rid} status=DRY-RUN")
            continue
        if opts.mode == "runs":
            cancel = gh("api", "-X", "POST",
                        f"repos/{repo_arg}/actions/runs/{rid}/cancel")
            _report_cancel(rid, cancel, opts)
        elif opts.mode == "jobs":
            jobs_resp = gh_json("api",
                               f"repos/{repo_arg}/actions/runs/{rid}/jobs?per_page=100")
            if not isinstance(jobs_resp, dict):
                continue
            for j in jobs_resp.get("jobs", []):
                if not isinstance(j, dict) or j.get("status") == "completed":
                    continue
                jid = j.get("id")
                if not isinstance(jid, int):
                    continue
                cancel = gh("api", "-X", "POST",
                            f"repos/{repo_arg}/actions/jobs/{jid}/cancel")
                _report_cancel(jid, cancel, opts)
    return attempts


def _report_cancel(item_id: int, res: GhResult, opts: CancelOptions) -> None:
    if res.ok:
        print(f"CANCEL  id={item_id} status=cancelled")
        return
    err = (res.stderr or "").strip().splitlines()
    err_first = err[0] if err else "unknown"
    if "HTTP 403" in res.stderr or "Resource not accessible" in res.stderr:
        print(f"CANCEL  id={item_id} status=permission_denied  ({err_first})")
        if opts.require:
            sys.exit(EXIT_REQUIRED_FAIL)
    elif "HTTP 404" in res.stderr or "HTTP 422" in res.stderr:
        print(f"CANCEL  id={item_id} status=already_completed")
    else:
        print(f"CANCEL  id={item_id} status=error  ({err_first})")
        if opts.require:
            sys.exit(EXIT_REQUIRED_FAIL)


def _resolve_origin_repo() -> str | None:
    res = gh("repo", "view", "--json", "nameWithOwner")
    if not res.ok:
        return None
    try:
        return json.loads(res.stdout).get("nameWithOwner")
    except json.JSONDecodeError:
        return None


# ---------- main poll loop ----------------------------------------------------

def _exit_after_cancel(code: int, on_label: str, pr: int, repo: str | None,
                      head_sha: str, opts: CancelOptions) -> None:
    """Cancel (if enabled) then exit `code`."""
    if on_label in opts.on or "always" in opts.on:
        cancel_pr_runs(pr, repo, head_sha, opts)
    sys.exit(code)


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
    args = ["run", "view", str(run_id), "--json",
            "jobs,status,conclusion,createdAt,updatedAt,workflowName"]
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
    counts = {"total": len(jobs), "queued": 0, "in_progress": 0,
              "completed": 0, "failed": 0}
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
    percent = int(round((counts["completed"] / total) * 100))
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


def emit_progress_report(pr: int, repo: str | None,
                          snapshot: "PRSnapshot",
                          checks: list["CheckRow"]) -> None:
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


def watch(pr: int, repo: str | None, interval: int, timeout: int,
         require_pattern: str | None, opts: CancelOptions) -> int:
    deadline = time.monotonic() + timeout
    review_state = ReviewState()
    snapshot: PRSnapshot | None = None
    required_names: set[str] | None = None
    # Initial snapshot — bail fast if the PR isn't open.
    snapshot = PRSnapshot.fetch(pr, repo)
    if snapshot is None:
        print(f"ERROR  could not fetch PR #{pr}", file=sys.stderr)
        return EXIT_PR_CLOSED
    if snapshot.state in {"MERGED", "CLOSED"}:
        print(f"PR-STATE  #{pr} state={snapshot.state}")
        _exit_after_cancel(EXIT_PR_CLOSED, "closed", pr, repo,
                          snapshot.head_sha, opts)

    # Resolve required check names (best-effort).
    repo_for_protection = repo or _resolve_origin_repo()
    if repo_for_protection:
        required_names = fetch_required_check_names(repo_for_protection, snapshot.base_ref)

    require_re = re.compile(require_pattern) if require_pattern else None

    while True:
        if time.monotonic() >= deadline:
            print(f"TIMEOUT  after {timeout}s")
            _exit_after_cancel(EXIT_TIMEOUT, "timeout", pr, repo,
                              snapshot.head_sha, opts)

        # 1. PR state first — break instantly on merge/close.
        fresh = PRSnapshot.fetch(pr, repo)
        if fresh is not None and fresh.state != "OPEN":
            print(f"PR-STATE  #{pr} state={fresh.state}")
            code = EXIT_GREEN if fresh.state == "MERGED" else EXIT_PR_CLOSED
            label = "closed"
            _exit_after_cancel(code, label, pr, repo, fresh.head_sha, opts)
        if fresh is not None:
            snapshot = fresh

        # 2. Review activity — if NEW since last check, exit 2.
        if review_state.update(pr, repo):
            print("REVIEW  new review activity")
            _exit_after_cancel(EXIT_REVIEW_ACTIVITY, "review", pr, repo,
                              snapshot.head_sha, opts)

        # 3. Check rollup.
        checks = fetch_checks(pr, repo)
        pending = [c for c in checks if c.bucket == "pending"]
        failing = [c for c in checks if c.bucket == "fail"]

        # Per-job progress reporting (#440). Emits structured JSONL on
        # stdout + a human-readable per-job line block on stderr so the
        # observer can answer "what's actually happening?" without
        # dropping out to `gh run view` manually.
        try:
            emit_progress_report(pr, repo, snapshot, checks)
        except Exception as exc:  # noqa: BLE001
            # Progress reporting is advisory; never let it break the
            # primary watch loop.
            print(f"NOTE  progress report failed: {exc}", file=sys.stderr)

        # Classify each failing check as required or advisory.
        for c in failing:
            if not _is_required(c, required_names, require_re):
                continue
            # First failing required check → bail.
            report = _build_failure_report(c, repo_for_protection)
            print(report.render())
            _exit_after_cancel(EXIT_REQUIRED_FAIL, "fail", pr, repo,
                              snapshot.head_sha, opts)

        # 4. All reported AND mergeable green path.
        if not pending and snapshot.mergeable == "MERGEABLE":
            for c in failing:  # surface advisory failures for context
                print(f"ADVISORY-FAIL  {c.name} (not in required set)")
            print(f"GREEN  #{pr} all required checks passed")
            _exit_after_cancel(EXIT_GREEN, "always" if "always" in opts.on else "never",
                              pr, repo, snapshot.head_sha, opts)

        time.sleep(interval)


def _is_required(c: CheckRow, required: set[str] | None,
                require_re: re.Pattern[str] | None) -> bool:
    if require_re is not None:
        return bool(require_re.search(c.name))
    if required is None:
        # No protection AND no allowlist: treat every check as required so
        # we never silently merge with a regression.
        return True
    return c.name in required


def _build_failure_report(c: CheckRow, repo: str | None) -> FailureReport:
    run_id = _extract_run_id_from_link(c.link)
    first_err, classifier = ("", None)
    if run_id and repo:
        first_err, classifier = classify_failure(repo, run_id, c.job_id)
    return FailureReport(check=c, run_id=run_id, first_error=first_err,
                         classifier=classifier)


def _extract_run_id_from_link(link: str | None) -> str | None:
    if not link:
        return None
    m = re.search(r"/actions/runs/(\d+)", link)
    return m.group(1) if m else None


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="pr_merge_watch",
        description="Fail-fast PR-check waiter for clud (issue #408).",
    )
    p.add_argument("pr_number", type=int, help="PR number to watch")
    p.add_argument("--repo", help="owner/name (defaults to current repo's origin)")
    p.add_argument("--interval", type=int, default=60,
                   help="seconds between polls (default 60)")
    p.add_argument("--timeout", type=int, default=3600,
                   help="overall wait cap in seconds (default 3600 = 60 min)")
    p.add_argument("--require", default=None,
                   help="regex of check names to treat as required when branch "
                        "protection is absent or inaccessible")
    # Cancellation control
    p.add_argument("--cancel-on", default=",".join(sorted(CANCEL_ON_DEFAULTS)),
                   help="comma-separated subset of "
                        f"{sorted(CANCEL_ON_CHOICES)} (default: fail,review,timeout,closed)")
    p.add_argument("--cancel-mode", default="runs", choices=sorted(CANCEL_MODE_CHOICES),
                   help="granularity of cancellation (default: runs)")
    p.add_argument("--cancel-timeout", type=int, default=30,
                   help="seconds to wait for cancellations to settle (default 30)")
    p.add_argument("--no-cancel", action="store_true",
                   help="shortcut for --cancel-on=never")
    p.add_argument("--require-cancel", action="store_true",
                   help="turn cancellation API errors into hard failures")
    p.add_argument("--dry-run-cancel", action="store_true",
                   help="list workflow runs that would be cancelled without POSTing")
    p.add_argument("--ignore-permission-errors", dest="ignore_perm",
                   action="store_true", default=True,
                   help="warn + continue on cancel 403s (default; --no-ignore-permission-errors flips)")
    p.add_argument("--no-ignore-permission-errors", dest="ignore_perm",
                   action="store_false")
    p.add_argument("--no-retry", action="store_true",
                   help="disable backoff/retry on cancel API calls")
    return p.parse_args(argv)


def _resolve_cancel_options(ns: argparse.Namespace) -> CancelOptions:
    raw = "never" if ns.no_cancel else ns.cancel_on
    parts = {p.strip() for p in raw.split(",") if p.strip()}
    invalid = parts - CANCEL_ON_CHOICES
    if invalid:
        raise SystemExit(f"--cancel-on: invalid values {sorted(invalid)}; "
                        f"choices are {sorted(CANCEL_ON_CHOICES)}")
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
    # Honor an env override for testing.
    if os.environ.get("CLUD_PR_MERGE_WATCH_DRY_RUN") == "1":
        print(f"DRY-RUN pr={ns.pr_number} repo={ns.repo or 'origin'} "
              f"interval={ns.interval} timeout={ns.timeout} "
              f"require={ns.require or 'branch-protection'} cancel_on={sorted(opts.on)}")
        return EXIT_GREEN
    return watch(ns.pr_number, ns.repo, ns.interval, ns.timeout, ns.require, opts)


if __name__ == "__main__":
    sys.exit(main())
