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

Immediately announces a durable repo-local JSONL log under
`.clud/logs/pr-merge-watch/`. The first record is a UTC `START`; later
records use monotonic seconds relative to that start.

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
from datetime import UTC, datetime
from pathlib import Path
from typing import TextIO

EXIT_GREEN = 0
EXIT_REQUIRED_FAIL = 1
EXIT_REVIEW_ACTIVITY = 2
EXIT_PR_CLOSED = 3
EXIT_TIMEOUT = 4

CANCEL_ON_CHOICES = {"fail", "review", "timeout", "closed", "always", "never"}
CANCEL_ON_DEFAULTS = {"fail", "review", "timeout", "closed"}
CANCEL_MODE_CHOICES = {"runs", "jobs", "none"}


def _utc_now() -> datetime:
    return datetime.now(UTC)


def _utc_text(value: datetime) -> str:
    return value.astimezone(UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _watch_root() -> Path:
    result = subprocess.run(
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


# First-error classifier patterns. Applied to up to ~200 lines of
# `gh run view --log-failed`. Order matters: first match wins.
CLASSIFIERS: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"^Diff in .*?:\d+:|run `cargo fmt", re.MULTILINE), "rustfmt drift"),
    (re.compile(r"^error: .*?clippy::|warning:.*?clippy::", re.MULTILINE), "clippy warning"),
    (re.compile(r"^error\[E\d+\]:|^error: could not compile", re.MULTILINE), "compile error"),
    (
        re.compile(r"^thread .*? panicked at|FAILED \(\d+\)|test result: FAILED", re.MULTILINE),
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


def gh(*args: str, check: bool = False) -> GhResult:
    """Run gh with the supplied args; return the captured outcome."""
    res = subprocess.run(["gh", *args], capture_output=True, text=True)
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
    def fetch(cls, pr: int, repo: str | None) -> PRSnapshot | None:
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


def classify_failure(repo: str, run_id: str, job_id: str | None) -> tuple[str, str | None]:
    """Fetch `gh run view --log-failed` for the run/job; classify the first error.

    Returns (first_error_line, classifier_label).
    """
    args = ["run", "view", run_id, "--repo", repo, "--log-failed"]
    if job_id:
        args = ["run", "view", run_id, "--repo", repo, "--job", job_id, "--log-failed"]
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
      number state mergeable headRefOid baseRefName
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
        ),
        checks=checks,
        human_review_ids=human_ids,
        coderabbit_probe=probe,
        coderabbit=observation,
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


def cancel_pr_runs(
    pr: int,
    repo: str | None,
    head_sha: str,
    opts: CancelOptions,
    log: WatchLog | None = None,
) -> int:
    """Cancel non-completed workflow runs on the PR's head SHA.

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
    runs_resp = gh_json("api", f"repos/{repo_arg}/actions/runs?head_sha={head_sha}&per_page=100")
    if not isinstance(runs_resp, dict):
        if log:
            log.emit("api_degraded", source="cancel_runs", reason="fetch_failed")
        return 0
    runs = runs_resp.get("workflow_runs", [])
    if not isinstance(runs, list):
        if log:
            log.emit("api_degraded", source="cancel_runs", reason="malformed_payload")
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
    err = (res.stderr or "").strip().splitlines()
    err_first = err[0] if err else "unknown"
    if "HTTP 403" in res.stderr or "Resource not accessible" in res.stderr:
        print(f"CANCEL  id={item_id} status=permission_denied  ({err_first})")
        if log:
            log.emit(
                "cancel_item",
                mode=mode,
                item_id=item_id,
                status="permission_denied",
                required=opts.require,
            )
    elif "HTTP 404" in res.stderr or "HTTP 422" in res.stderr:
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
) -> None:
    if on_label in opts.on or "always" in opts.on:
        attempts = cancel_pr_runs(pr, repo, head_sha, opts, log)
        if log:
            log.emit("cancel", trigger=on_label, attempts=attempts, mode=opts.mode)


def _finish_exit(code: int, reason: str, log: WatchLog | None = None) -> None:
    if log:
        log.emit("EXIT", code=code, reason=reason)
        log.close()
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
) -> int:
    deadline = time.monotonic() + timeout
    snapshot: PRSnapshot | None = None
    required_names: set[str] | None = None
    # Initial snapshot — bail fast if the PR isn't open.
    snapshot = PRSnapshot.fetch(pr, repo)
    if snapshot is None:
        print(f"ERROR  could not fetch PR #{pr}", file=sys.stderr)
        if log:
            log.emit("api_degraded", source="pr_snapshot", reason="fetch_failed")
        return EXIT_PR_CLOSED
    if snapshot.state in {"MERGED", "CLOSED"}:
        print(f"PR-STATE  #{pr} state={snapshot.state}")
        if log:
            log.emit("pr_state", state=snapshot.state, head_sha=snapshot.head_sha)
        code = EXIT_GREEN if snapshot.state == "MERGED" else EXIT_PR_CLOSED
        label = "merged" if snapshot.state == "MERGED" else "closed"
        _exit_after_cancel(code, label, pr, repo, snapshot.head_sha, opts, log)

    # Resolve required check names (best-effort).
    repo_for_protection = repo or _resolve_origin_repo()
    if repo_for_protection:
        required_names = fetch_required_check_names(repo_for_protection, snapshot.base_ref)

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
        gate = (
            fetch_gate_snapshot(
                repo_for_protection,
                pr,
                include_coderabbit=not coderabbit_probe_complete or review_state.coderabbit_enabled,
            )
            if repo_for_protection
            else None
        )
        if gate is None:
            print("NOTE  gate snapshot unavailable; retrying", file=sys.stderr, flush=True)
            if log:
                log.emit("api_degraded", source="gate_snapshot", reason="fetch_failed")
            _sleep_remaining_interval(poll_started, interval)
            continue
        snapshot = gate.pr
        if snapshot.state != "OPEN":
            print(f"PR-STATE  #{pr} state={snapshot.state}")
            if log:
                log.emit("pr_state", state=snapshot.state, head_sha=snapshot.head_sha)
            code = EXIT_GREEN if snapshot.state == "MERGED" else EXIT_PR_CLOSED
            label = "merged" if snapshot.state == "MERGED" else "closed"
            _exit_after_cancel(code, label, pr, repo, snapshot.head_sha, opts, log)

        # 1. Check rollup and required-failure classification.
        checks = gate.checks
        pending = [c for c in checks if c.bucket == "pending"]
        failing = [c for c in checks if c.bucket in {"fail", "cancel"}]
        counts = check_counts(checks)
        if log:
            log.emit("checks", checks=counts)
            elapsed = round(max(0.0, time.monotonic() - log.started_monotonic), 2)
            print(
                f"{elapsed:.2f} {counts['succeeded']} succeeded, "
                f"{counts['failed']} failed, {counts['pending']} pending",
                file=sys.stderr,
                flush=True,
            )

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
            # Cancel current-head work before the advisory log probe, which
            # can be slower than the cancellation API for large failed runs.
            _cancel_for_exit("fail", pr, repo, snapshot.head_sha, opts, log)
            report = _build_failure_report(c, repo_for_protection)
            print(report.render())
            if log and (report.first_error or report.classifier):
                log.emit(
                    "failure_diagnostic",
                    check_name=c.name,
                    first_error=report.first_error,
                    classifier=report.classifier,
                )
            _finish_exit(EXIT_REQUIRED_FAIL, "fail", log)

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
        if not pending and snapshot.mergeable == "MERGEABLE":
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


def _is_required(
    c: CheckRow, required: set[str] | None, require_re: re.Pattern[str] | None
) -> bool:
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
    return FailureReport(check=c, run_id=run_id, first_error=first_err, classifier=classifier)


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
    p.add_argument("--interval", type=int, default=60, help=argparse.SUPPRESS)
    p.add_argument(
        "--timeout",
        type=int,
        default=3600,
        help="overall wait cap in seconds (default 3600 = 60 min)",
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
    code = watch(ns.pr_number, ns.repo, ns.interval, ns.timeout, ns.require, opts, log)
    if not log.closed:
        log.emit("EXIT", code=code, reason="return")
        log.close()
    return code


if __name__ == "__main__":
    sys.exit(main())
