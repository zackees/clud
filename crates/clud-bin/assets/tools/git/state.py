#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""state.py — a checkout's whole git state in one call (#1339 item 7).

Usage:
  state.py [--commits N] [--json]

/grind agents probe a checkout with `git status`, `git branch`, `git log`,
`git stash list` and `git worktree list`, one call each, again and again.
This prints all of it at once: the branch, its upstream with ahead/behind,
`status --porcelain` entries, the last N commits (default 3), stashes and
worktrees. It never fetches, so the upstream counts are as of the last fetch.

Run through clud's `tool run` subcommand:
  clud tool run git/state.py

Exit codes:
  0  printed
  1  usage error
  2  not a git checkout, or a git call failed
"""

from __future__ import annotations

import argparse
import json
import sys

from running_process import PIPE, RunningProcess, TimeoutExpired

TIMEOUT = 60.0


def git(*args: str) -> tuple[int, str]:
    try:
        res = RunningProcess.run(
            ["git", *args], capture_output=True, stderr=PIPE, text=True, timeout=TIMEOUT, check=False
        )
    except (TimeoutError, TimeoutExpired):
        return 124, ""
    except FileNotFoundError:
        return 127, ""
    return res.returncode, res.stdout or ""


def parse_status(text: str) -> dict:
    """Parse `git status --porcelain=v2 --branch` into branch facts and entries."""
    state: dict = {
        "branch": None,
        "head": None,
        "upstream": None,
        "ahead": None,
        "behind": None,
        "changes": [],
    }
    for line in text.splitlines():
        if line.startswith("# branch.oid "):
            oid = line.split(" ", 2)[2]
            state["head"] = None if oid == "(initial)" else oid[:12]
        elif line.startswith("# branch.head "):
            head = line.split(" ", 2)[2]
            state["branch"] = None if head == "(detached)" else head
        elif line.startswith("# branch.upstream "):
            state["upstream"] = line.split(" ", 2)[2]
        elif line.startswith("# branch.ab "):
            ahead, behind = line.split(" ")[2:4]
            state["ahead"] = int(ahead.lstrip("+"))
            state["behind"] = int(behind.lstrip("-"))
        elif line.startswith(("1 ", "2 ")):
            fields = line.split(" ")
            path = line.split(" ", 9 if line[0] == "2" else 8)[-1]
            state["changes"].append({"xy": fields[1], "path": path.split("\t")[0]})
        elif line.startswith("u "):
            state["changes"].append({"xy": "UU", "path": line.split(" ", 10)[-1]})
        elif line.startswith("? "):
            state["changes"].append({"xy": "??", "path": line[2:]})
    return state


def parse_worktrees(text: str) -> list[dict]:
    """Parse `git worktree list --porcelain`."""
    trees: list[dict] = []
    for line in text.splitlines():
        if line.startswith("worktree "):
            trees.append({"path": line[len("worktree ") :], "branch": None})
        elif line.startswith("branch ") and trees:
            trees[-1]["branch"] = line[len("branch ") :].removeprefix("refs/heads/")
        elif line == "detached" and trees:
            trees[-1]["branch"] = None
    return trees


def render(state: dict) -> str:
    branch = state["branch"] or f"(detached at {state['head']})"
    lines = [f"branch: {branch}"]
    if state["upstream"]:
        lines.append(
            f"upstream: {state['upstream']} ahead {state['ahead']} behind {state['behind']}"
        )
    else:
        lines.append("upstream: none")
    lines.append(f"changes: {len(state['changes'])}")
    lines.extend(f"  {c['xy']} {c['path']}" for c in state["changes"])
    lines.append("commits:")
    lines.extend(f"  {c}" for c in state["commits"])
    lines.append(f"stashes: {len(state['stashes'])}")
    lines.extend(f"  {s}" for s in state["stashes"])
    lines.append(f"worktrees: {len(state['worktrees'])}")
    lines.extend(f"  {w['path']} [{w['branch'] or 'detached'}]" for w in state["worktrees"])
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="state.py", description=__doc__.split("\n")[0])
    parser.add_argument("--commits", type=int, default=3)
    parser.add_argument("--json", action="store_true")
    try:
        args = parser.parse_args(argv)
    except SystemExit:
        return 1
    rc, status = git("status", "--porcelain=v2", "--branch")
    if rc != 0:
        print("state.py: not a git checkout, or git status failed", file=sys.stderr)
        return 2
    state = parse_status(status)
    _, log = git("log", f"-{max(args.commits, 0)}", "--format=%h %s")
    _, stashes = git("stash", "list", "--format=%gd %s")
    _, worktrees = git("worktree", "list", "--porcelain")
    state["commits"] = log.splitlines()
    state["stashes"] = stashes.splitlines()
    state["worktrees"] = parse_worktrees(worktrees)
    print(json.dumps(state, indent=2) if args.json else render(state))
    return 0


if __name__ == "__main__":
    sys.exit(main())
