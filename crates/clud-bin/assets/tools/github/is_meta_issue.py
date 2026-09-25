#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""is_meta_issue.py — decide whether a GitHub issue is a meta (parent) issue.

Usage:
  is_meta_issue.py <issue> [--repo OWNER/REPO]

<issue> is an issue number or a https://github.com/OWNER/REPO/issues/N URL
(the URL supplies the repo). Without --repo or a URL, the repo is resolved
with `gh repo view --json nameWithOwner`.

An issue is meta when it has native GitHub sub-issues or its body carries a
task list (`- [ ]` / `- [x]` lines) referencing other issues or PRs.

Prints JSON on stdout:
  {"meta": bool, "sub_issues": [{"number": int, "state": str}],
   "task_list_refs": [int | "owner/repo#N", ...]}

Invoked via `"$CLUD_EXE" tool run github/is_meta_issue.py …`.

Exit codes:
  0  answered (meta true or false)
  1  usage error
  2  a gh call failed (nonzero exit, timeout, unparseable JSON); nothing is
     printed on stdout, so a failure never reads as `meta: false`
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass

from running_process import PIPE, RunningProcess, TimeoutExpired

EXIT_OK = 0
EXIT_USAGE = 1
EXIT_GH_FAILED = 2

GH_TIMEOUT = 60.0

_TASK_LINE = re.compile(r"^\s*[-*]\s+\[[ xX]\]\s+")
_ISSUE_URL = re.compile(r"https?://github\.com/([\w.-]+)/([\w.-]+)/(?:issues|pull)/(\d+)")
_REF = re.compile(r"(?<![\w/.-])(?:([\w.-]+)/([\w.-]+))?#(\d+)\b")
_ARG_URL = re.compile(r"^https?://github\.com/([\w.-]+)/([\w.-]+)/issues/(\d+)/?(?:[?#].*)?$")


class GhError(Exception):
    """A gh call failed; the message is user-facing."""


class UsageError(Exception):
    """The command line was invalid."""


@dataclass
class GhResult:
    """Outcome of one gh CLI call."""

    exit_code: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.exit_code == 0


def gh(*args: str, check: bool = False, timeout: float | None = GH_TIMEOUT) -> GhResult:
    """Run gh with the supplied args; return the captured outcome."""
    try:
        # stderr=PIPE: running-process merges stderr into stdout otherwise (#1175).
        res = RunningProcess.run(
            ["gh", *args], capture_output=True, stderr=PIPE, text=True, timeout=timeout
        )
    except (TimeoutError, TimeoutExpired):
        return GhResult(124, "", f"gh {' '.join(args)} timed out after {timeout}s")
    stdout = res.stdout or ""
    stderr = res.stderr or ""
    if check and res.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {stderr.strip()}")
    return GhResult(res.returncode, stdout, stderr)


def _gh_checked(*args: str) -> str:
    r = gh(*args)
    if not r.ok:
        raise GhError(
            f"gh {' '.join(args)} failed (exit {r.exit_code}): {(r.stderr or '').strip()}"
        )
    return r.stdout or ""


def _parse_json_stream(text: str, what: str) -> list[object]:
    """Parse one or more concatenated JSON documents (`--paginate` output)."""
    decoder = json.JSONDecoder()
    docs: list[object] = []
    idx = 0
    text = text.strip()
    try:
        while idx < len(text):
            doc, end = decoder.raw_decode(text, idx)
            docs.append(doc)
            idx = end
            while idx < len(text) and text[idx].isspace():
                idx += 1
    except json.JSONDecodeError as exc:
        raise GhError(f"could not parse {what} JSON from gh: {exc}") from exc
    if not docs:
        raise GhError(f"gh returned no {what} JSON")
    return docs


def parse_sub_issues(text: str) -> list[dict]:
    """Flatten paginated sub_issues output into [{number, state}]."""
    out: list[dict] = []
    for doc in _parse_json_stream(text, "sub_issues"):
        if not isinstance(doc, list):
            raise GhError("unexpected sub_issues JSON shape from gh")
        for item in doc:
            if not isinstance(item, dict) or not isinstance(item.get("number"), int):
                raise GhError("unexpected sub_issues entry from gh")
            out.append({"number": item["number"], "state": str(item.get("state", ""))})
    return out


def parse_task_list_refs(body: str | None) -> list[int | str]:
    """Collect issue refs from GitHub task-list lines, de-duplicated in order."""
    refs: list[int | str] = []
    seen: set[int | str] = set()

    def add(ref: int | str) -> None:
        if ref not in seen:
            seen.add(ref)
            refs.append(ref)

    for line in (body or "").splitlines():
        if not _TASK_LINE.match(line):
            continue
        rest = _TASK_LINE.sub("", line, count=1)
        found: list[tuple[int, int | str]] = []
        for m in _ISSUE_URL.finditer(rest):
            found.append((m.start(), f"{m.group(1)}/{m.group(2)}#{m.group(3)}"))
        scrubbed = _ISSUE_URL.sub(lambda m: " " * len(m.group(0)), rest)
        for m in _REF.finditer(scrubbed):
            if m.group(1):
                found.append((m.start(), f"{m.group(1)}/{m.group(2)}#{m.group(3)}"))
            else:
                found.append((m.start(), int(m.group(3))))
        for _, ref in sorted(found, key=lambda x: x[0]):
            add(ref)
    return refs


def _localize(refs: list[int | str], repo: str | None) -> list[int | str]:
    """Turn `repo#N` refs for the issue's own repo into plain ints."""
    if not repo:
        return refs
    out: list[int | str] = []
    seen: set[int | str] = set()
    prefix = f"{repo.lower()}#"
    for ref in refs:
        if isinstance(ref, str) and ref.lower().startswith(prefix):
            ref = int(ref[len(prefix) :])
        if ref not in seen:
            seen.add(ref)
            out.append(ref)
    return out


def build_result(sub_issues_json: str, body: str | None, repo: str | None = None) -> dict:
    """Combine gh outputs into the printed verdict."""
    sub_issues = parse_sub_issues(sub_issues_json)
    refs = _localize(parse_task_list_refs(body), repo)
    return {
        "meta": bool(sub_issues or refs),
        "sub_issues": sub_issues,
        "task_list_refs": refs,
    }


def parse_issue_arg(issue: str, repo: str | None) -> tuple[str | None, int]:
    """Return (repo or None, number) from a number or issue URL."""
    issue = issue.strip()
    m = _ARG_URL.match(issue)
    if m:
        return (repo or f"{m.group(1)}/{m.group(2)}"), int(m.group(3))
    text = issue.lstrip("#")
    if text.isdigit() and int(text) > 0:
        return repo, int(text)
    raise UsageError(f"not an issue number or issue URL: {issue!r}")


def _resolve_repo() -> str:
    out = _gh_checked("repo", "view", "--json", "nameWithOwner")
    try:
        name = json.loads(out)["nameWithOwner"]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        raise GhError(f"could not parse `gh repo view` output: {exc}") from exc
    if not isinstance(name, str) or "/" not in name:
        raise GhError("`gh repo view` returned no nameWithOwner")
    return name


class _Parser(argparse.ArgumentParser):
    def error(self, message: str) -> None:  # type: ignore[override]
        raise UsageError(message)


def main(argv: list[str] | None = None) -> int:
    parser = _Parser(prog="is_meta_issue.py", description="Is a GitHub issue a meta issue?")
    parser.add_argument("issue", help="issue number or https://github.com/o/r/issues/N URL")
    parser.add_argument("--repo", help="OWNER/REPO (defaults to the URL's or the cwd's repo)")
    try:
        ns = parser.parse_args(sys.argv[1:] if argv is None else argv)
        if ns.repo is not None and not re.fullmatch(r"[\w.-]+/[\w.-]+", ns.repo):
            raise UsageError(f"--repo must be OWNER/REPO, got {ns.repo!r}")
        repo, number = parse_issue_arg(ns.issue, ns.repo)
    except UsageError as exc:
        print(f"usage error: {exc}", file=sys.stderr)
        parser.print_usage(sys.stderr)
        return EXIT_USAGE

    try:
        if repo is None:
            repo = _resolve_repo()
        base = f"repos/{repo}/issues/{number}"
        sub_json = _gh_checked("api", f"{base}/sub_issues", "--paginate")
        issue_text = _gh_checked("api", base)
        try:
            issue = json.loads(issue_text)
        except json.JSONDecodeError as exc:
            raise GhError(f"could not parse issue JSON from gh: {exc}") from exc
        if not isinstance(issue, dict):
            raise GhError("unexpected issue JSON shape from gh")
        result = build_result(sub_json, issue.get("body"), repo)
    except GhError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_GH_FAILED

    print(json.dumps(result))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
