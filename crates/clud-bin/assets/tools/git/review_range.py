#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""review_range.py — resolve the diff range a code review must read (#1301).

Usage:
  review_range.py [--pr N] [--repo OWNER/REPO] [--base REF] [--max-files N]
                  [--max-lines N] [--no-fetch]

A review that diffs against the branch's upstream tracking ref takes in the
whole rebase delta when that ref is stale (a rebased, force-pushed branch): a
4-file PR became 211 files and 13,488 lines. This tool never uses the
tracking ref. It pins the range to SHAs, once, for every bucket diff:

  * `--pr N`: `baseRefOid` / `headRefOid` from `gh pr view`, and the PR's own
    diffstat, so the local range can be checked against it.
  * otherwise: the merge-base of HEAD with the repository's default branch
    (`origin/HEAD`, else `origin/main` / `origin/master`), fetched first.

Prints JSON on stdout:
  {"source": "pr"|"local", "base": "<sha>", "head": "<sha>",
   "merge_base": "<sha>", "range": "<merge_base>...<head>",
   "files": int, "insertions": int, "deletions": int,
   "pr_files": int|null, "pr_insertions": int|null, "pr_deletions": int|null,
   "oversize": bool}

`oversize` is true when the range exceeds `--max-files` (default 50) or
`--max-lines` (default 3000) AND does not match the PR's own diffstat. An
oversize range means stop before reading any content and report both
diffstats; when a large range really is intended, use a file inventory plus
targeted hunks.

Run through clud's `tool run` subcommand.

Exit codes:
  0  resolved
  1  usage error
  2  a git or gh call failed; nothing is printed on stdout
  3  resolved but oversize (the JSON is still printed)
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass

from running_process import PIPE, RunningProcess, TimeoutExpired

EXIT_OK = 0
EXIT_USAGE = 1
EXIT_FAILED = 2
EXIT_OVERSIZE = 3

TIMEOUT = 120.0
DEFAULT_MAX_FILES = 50
DEFAULT_MAX_LINES = 3000


class ToolError(Exception):
    """A git or gh call failed; the message is user-facing."""


@dataclass(frozen=True)
class Result:
    exit_code: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.exit_code == 0


def _run(argv: list[str], timeout: float = TIMEOUT) -> Result:
    try:
        res = RunningProcess.run(
            argv, capture_output=True, stderr=PIPE, text=True, timeout=timeout, check=False
        )
    except (TimeoutError, TimeoutExpired):
        return Result(124, "", f"{' '.join(argv)} timed out after {timeout}s")
    except FileNotFoundError:
        return Result(127, "", f"{argv[0]}: command not found")
    return Result(res.returncode, res.stdout or "", res.stderr or "")


def git(*args: str) -> Result:
    return _run(["git", *args])


def gh(*args: str) -> Result:
    return _run(["gh", *args])


def _checked(result: Result, what: str) -> str:
    if not result.ok:
        raise ToolError(f"{what} failed (exit {result.exit_code}): {result.stderr.strip()}")
    return result.stdout.strip()


def default_base() -> str:
    """`origin/<default branch>`: origin/HEAD, else origin/main, else origin/master."""
    head = git("symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD")
    if head.ok and head.stdout.strip():
        return head.stdout.strip()
    for candidate in ("origin/main", "origin/master"):
        if git("rev-parse", "--verify", "--quiet", candidate).ok:
            return candidate
    raise ToolError("cannot find the default branch (no origin/HEAD, origin/main or origin/master)")


def diffstat(merge_base: str, head: str) -> tuple[int, int, int]:
    """(files, insertions, deletions) of `merge_base..head`; binary files count 0 lines."""
    text = _checked(git("diff", "--numstat", merge_base, head), "git diff --numstat")
    files = insertions = deletions = 0
    for line in text.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        files += 1
        insertions += int(parts[0]) if parts[0].isdigit() else 0
        deletions += int(parts[1]) if parts[1].isdigit() else 0
    return files, insertions, deletions


def resolve(
    *,
    pr: str | None = None,
    repo: str | None = None,
    base: str | None = None,
    max_files: int = DEFAULT_MAX_FILES,
    max_lines: int = DEFAULT_MAX_LINES,
    fetch: bool = True,
) -> dict[str, object]:
    pr_stat: tuple[int, int, int] | None = None
    if pr:
        args = ["pr", "view", pr, "--json", "baseRefOid,headRefOid,changedFiles,additions,deletions"]
        if repo:
            args += ["--repo", repo]
        info = json.loads(_checked(gh(*args), "gh pr view"))
        base_sha, head_sha = info["baseRefOid"], info["headRefOid"]
        if fetch:
            # Best effort: the objects may already be local.
            git("fetch", "--quiet", "origin", base_sha, head_sha)
        pr_stat = (info["changedFiles"], info["additions"], info["deletions"])
        source = "pr"
    else:
        base_ref = base or default_base()
        if fetch and base_ref.startswith("origin/"):
            git("fetch", "--quiet", "origin", base_ref.removeprefix("origin/"))
        base_sha = _checked(git("rev-parse", "--verify", base_ref), f"git rev-parse {base_ref}")
        head_sha = _checked(git("rev-parse", "--verify", "HEAD"), "git rev-parse HEAD")
        source = "local"
    merge_base = _checked(git("merge-base", base_sha, head_sha), "git merge-base")
    files, insertions, deletions = diffstat(merge_base, head_sha)
    matches_pr = pr_stat is not None and pr_stat == (files, insertions, deletions)
    big = files > max_files or (insertions + deletions) > max_lines
    return {
        "source": source,
        "base": base_sha,
        "head": head_sha,
        "merge_base": merge_base,
        "range": f"{merge_base}...{head_sha}",
        "files": files,
        "insertions": insertions,
        "deletions": deletions,
        "pr_files": pr_stat[0] if pr_stat else None,
        "pr_insertions": pr_stat[1] if pr_stat else None,
        "pr_deletions": pr_stat[2] if pr_stat else None,
        "oversize": big and not matches_pr,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="review_range", description=__doc__.split("\n")[0])
    parser.add_argument("--pr", help="PR number or URL; pins the PR's own base/head SHAs")
    parser.add_argument("--repo", help="OWNER/REPO for --pr")
    parser.add_argument("--base", help="base ref for a local range (default: the default branch)")
    parser.add_argument("--max-files", type=int, default=DEFAULT_MAX_FILES)
    parser.add_argument("--max-lines", type=int, default=DEFAULT_MAX_LINES)
    parser.add_argument("--no-fetch", action="store_true", help="do not fetch before resolving")
    try:
        ns = parser.parse_args(argv if argv is not None else sys.argv[1:])
    except SystemExit as exit_:
        return EXIT_USAGE if exit_.code not in (0, None) else EXIT_OK
    try:
        out = resolve(
            pr=ns.pr,
            repo=ns.repo,
            base=ns.base,
            max_files=ns.max_files,
            max_lines=ns.max_lines,
            fetch=not ns.no_fetch,
        )
    except (ToolError, KeyError, ValueError) as error:
        print(f"review_range: {error}", file=sys.stderr)
        return EXIT_FAILED
    print(json.dumps(out))
    return EXIT_OVERSIZE if out["oversize"] else EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
