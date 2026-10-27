#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["running-process==4.10.1"]
# ///
# managed-by: clud
"""Submit an exact GitHub PR head for merge, including protected merge queues.

Arguments: PR --repo OWNER/REPO --match-head-commit SHA

The repository and full head SHA are required. The base branch's merge queue
selects GitHub's enqueuePullRequest mutation, or `gh pr merge
--squash` for an ordinary branch. The tool never uses admin bypass. A zero
exit reports submission or an existing queue entry, not a completed merge;
follow it with the bundled github/pr_merge_watch.py tool.
An API or timeout error can leave the mutation outcome unknown: check the PR
before retrying. The command never retries mutations itself.
"""

from __future__ import annotations

import argparse
import json
import re
import sys

from running_process import PIPE, RunningProcess, TimeoutExpired

PR_QUERY = """query($owner:String!, $name:String!, $number:Int!) {
  repository(owner:$owner, name:$name) {
    pullRequest(number:$number) {
      id state isDraft headRefOid baseRefName mergeQueueEntry { id }
    }
  }
}"""
QUEUE_QUERY = """query($owner:String!, $name:String!, $base:String!) {
  repository(owner:$owner, name:$name) { mergeQueue(branch:$base) { id } }
}"""
ENQUEUE = """mutation($id:ID!, $sha:GitObjectID!) {
  enqueuePullRequest(input:{pullRequestId:$id, expectedHeadOid:$sha}) {
    mergeQueueEntry { id }
  }
}"""
_REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
_SHA = re.compile(r"[0-9a-fA-F]{40}\Z")


class SubmitError(Exception):
    """The PR could not be submitted safely."""


def gh_command(*args: str) -> str:
    """Run a single bounded gh command without retrying a mutation."""
    try:
        result = RunningProcess.run(
            ["gh", *args], capture_output=True, stderr=PIPE, text=True, timeout=60
        )
    except (TimeoutError, TimeoutExpired) as exc:
        raise SubmitError("gh timed out; inspect the PR before retrying") from exc
    if result.returncode:
        raise SubmitError(
            f"gh exited {result.returncode}: {(result.stderr or '').strip()}; "
            "inspect the PR before retrying"
        )
    return result.stdout or ""


def gh_json(*args: str) -> object:
    try:
        result = json.loads(gh_command(*args))
    except (ValueError, TypeError) as exc:
        raise SubmitError("gh returned invalid JSON; inspect the PR before retrying") from exc
    if isinstance(result, dict) and result.get("errors"):
        raise SubmitError("GitHub returned errors; inspect the PR before retrying")
    return result


def _pr(repo: str, number: int) -> dict:
    owner, name = repo.split("/", 1)
    payload = gh_json(
        "api", "--hostname", "github.com", "graphql", "-f", f"query={PR_QUERY}",
        "-f", f"owner={owner}", "-f", f"name={name}", "-F", f"number={number}",
    )
    try:
        pr = payload["data"]["repository"]["pullRequest"]
        if not isinstance(pr, dict) or not isinstance(pr["id"], str) or not pr["id"]:
            raise ValueError("invalid PR")
        return pr
    except (KeyError, TypeError, ValueError) as exc:
        raise SubmitError("GitHub returned no valid PR") from exc


def _has_queue(repo: str, base: str) -> bool:
    owner, name = repo.split("/", 1)
    payload = gh_json(
        "api", "--hostname", "github.com", "graphql", "-f", f"query={QUEUE_QUERY}",
        "-f", f"owner={owner}", "-f", f"name={name}", "-f", f"base={base}",
    )
    try:
        queue = payload["data"]["repository"]["mergeQueue"]
        if queue is not None and (
            not isinstance(queue, dict) or not isinstance(queue.get("id"), str) or not queue["id"]
        ):
            raise ValueError("invalid merge queue")
        return queue is not None
    except (KeyError, TypeError, ValueError) as exc:
        raise SubmitError("GitHub returned invalid branch queue state") from exc


def _queue_entry(pr: dict) -> object:
    if "mergeQueueEntry" not in pr:
        raise SubmitError("GitHub returned no PR merge queue entry state")
    entry = pr["mergeQueueEntry"]
    if entry is not None and (
        not isinstance(entry, dict) or not isinstance(entry.get("id"), str) or not entry["id"]
    ):
        raise SubmitError("GitHub returned an invalid merge queue entry")
    return entry


def submit(repo: str, number: int, sha: str) -> str:
    """Submit once; fail closed on a stale head or unreadable queue state."""
    if (not _REPO.fullmatch(repo) or any(part in {".", ".."} for part in repo.split("/"))
            or number <= 0 or not _SHA.fullmatch(sha)):
        raise SubmitError("provide OWNER/REPO, a positive PR number, and a full 40-digit head SHA")
    pr = _pr(repo, number)
    if pr.get("state") != "OPEN" or pr.get("isDraft") is not False:
        raise SubmitError("PR must be open and ready for review")
    head = pr.get("headRefOid")
    if not isinstance(head, str) or head.lower() != sha.lower():
        raise SubmitError("PR head changed; inspect it and use its full current SHA")
    entry = _queue_entry(pr)
    if entry is not None:
        return "already queued"
    base = pr.get("baseRefName")
    if not isinstance(base, str) or not base:
        raise SubmitError("GitHub returned no PR base branch")
    if not _has_queue(repo, base):
        gh_command(
            "pr", "merge", str(number), "--repo", f"github.com/{repo}",
            "--squash", "--match-head-commit", sha,
        )
        return "submitted"
    result = gh_json(
        "api", "--hostname", "github.com", "graphql", "-f", f"query={ENQUEUE}",
        "-f", f"id={pr['id']}", "-f", f"sha={sha}",
    )
    try:
        entry = result["data"]["enqueuePullRequest"]["mergeQueueEntry"]["id"]
        if not isinstance(entry, str) or not entry:
            raise ValueError("invalid queue entry")
    except (KeyError, TypeError, ValueError) as exc:
        raise SubmitError("enqueue result unreadable; inspect the PR before retrying") from exc
    return "enqueued"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pr", type=int)
    parser.add_argument("--repo", required=True, metavar="OWNER/REPO")
    parser.add_argument("--match-head-commit", required=True, metavar="SHA")
    args = parser.parse_args()
    try:
        result = submit(args.repo, args.pr, args.match_head_commit)
    except SubmitError as exc:
        print(f"pr_merge: {exc}", file=sys.stderr)
        return 1
    print(f"PR #{args.pr}: {result}; follow the PR to confirm it merged")
    return 0


if __name__ == "__main__":
    sys.exit(main())
