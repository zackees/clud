#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""patch.py — apply search/replace blocks across files in one call (#1339 item 5).

Usage:
  patch.py [--dry-run] [PATCH_FILE]      (reads stdin when PATCH_FILE is absent)

/grind agents made multi-site edits with throwaway Python scripts, which
bypass the per-task file list, and with `Edit` calls that failed on a string
that matched twice or not at all. This applies every block in one call, and
applies nothing unless every block is valid:

  === src/lib.rs
  <<<<<<< SEARCH
  fn old() {}
  =======
  fn new() {}
  >>>>>>> REPLACE

A `=== <path>` line names the file (relative to the current directory) for
the blocks after it. Each SEARCH text must occur exactly once in the file
as it stands after the earlier blocks. An empty SEARCH creates a file that
does not exist yet.

In a /grind run, the files must belong to a task the planner recorded for
this checkout (`clud grind-facts task`); anything else is refused. Outside a
run there is no file list.

Run through clud's `tool run` subcommand:
  "$CLUD_EXE" tool run code/patch.py <<'EOF'
  ...blocks...
  EOF

Exit codes:
  0  applied (or, with --dry-run, would apply)
  1  usage error or malformed patch
  2  refused: a block did not match exactly once, or a file is outside the task
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path

from running_process import PIPE, RunningProcess, TimeoutExpired

FILE = "=== "
SEARCH = "<<<<<<< SEARCH"
DIVIDER = "======="
REPLACE = ">>>>>>> REPLACE"


class PatchError(Exception):
    def __init__(self, message: str, code: int) -> None:
        super().__init__(message)
        self.code = code


@dataclass
class Block:
    path: str
    search: str
    replace: str
    line: int


def parse(text: str) -> list[Block]:
    blocks: list[Block] = []
    path = None
    lines = text.splitlines(keepends=True)
    i = 0
    while i < len(lines):
        line = lines[i].rstrip("\r\n")
        if line.startswith(FILE):
            path = line[len(FILE) :].strip()
            i += 1
            continue
        if line != SEARCH:
            if line.strip():
                raise PatchError(f"line {i + 1}: expected `{FILE}<path>` or `{SEARCH}`", 1)
            i += 1
            continue
        if path is None:
            raise PatchError(f"line {i + 1}: a block before any `{FILE}<path>` line", 1)
        start = i + 1
        parts: list[list[str]] = [[], []]
        part = 0
        i += 1
        while i < len(lines) and lines[i].rstrip("\r\n") != REPLACE:
            if lines[i].rstrip("\r\n") == DIVIDER and part == 0:
                part = 1
            else:
                parts[part].append(lines[i])
            i += 1
        if i == len(lines) or part == 0:
            raise PatchError(f"line {start}: block is missing `{DIVIDER}` or `{REPLACE}`", 1)
        blocks.append(Block(path, "".join(parts[0]), "".join(parts[1]), start))
        i += 1
    if not blocks:
        raise PatchError("no blocks", 1)
    return blocks


def git_root(cwd: Path) -> Path | None:
    try:
        res = RunningProcess.run(
            ["git", "rev-parse", "--show-toplevel"], cwd=cwd, stdout=PIPE, stderr=PIPE, timeout=30
        )
    except (OSError, TimeoutExpired):
        return None
    out = (res.stdout or "").strip()
    return Path(out).resolve() if res.returncode == 0 and out else None


def facts_file() -> Path | None:
    exe = os.environ.get("CLUD_EXE")
    if not exe or not os.environ.get("CLAUDE_CODE_SESSION_ID"):
        return None
    try:
        res = RunningProcess.run([exe, "grind-facts", "path"], stdout=PIPE, stderr=PIPE, timeout=30)
    except (OSError, TimeoutExpired):
        return None
    out = (res.stdout or "").strip()
    return Path(out) if res.returncode == 0 and out else None


def task_files(facts: dict, root: Path) -> set[Path] | None:
    """Every file a recorded task in `root` owns, or None when no task is recorded there."""
    entries = facts.get("repos") if isinstance(facts.get("repos"), list) else [facts]
    owned: set[Path] | None = None
    for entry in entries:
        tasks = entry.get("tasks") if isinstance(entry, dict) else None
        for task in tasks or []:
            worktree = Path(str(task.get("worktree", ""))).resolve()
            if worktree != root:
                continue
            owned = owned or set()
            owned.update((root / f).resolve() for f in task.get("files") or [])
    return owned


def allowed_files(cwd: Path) -> set[Path] | None:
    path = facts_file()
    root = git_root(cwd)
    if path is None or root is None or not path.is_file():
        return None
    try:
        facts = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return task_files(facts, root) if isinstance(facts, dict) else None


def read(path: Path) -> str:
    """The file exactly as stored: line endings are not translated."""
    with path.open(encoding="utf-8", newline="") as src:
        return src.read()


def with_crlf(text: str) -> str:
    """`text` in CRLF form, whatever endings it arrived with."""
    return text.replace("\r\n", "\n").replace("\n", "\r\n")


def crlf_variant(search: str, current: str) -> str | None:
    """`search` rewritten to CRLF when the file stores CRLF and the literal form misses.

    A patch arrives with whatever endings its writer used -- typically LF, even
    on Windows -- while the working tree may hold CRLF. Exact text always wins;
    this is only consulted after the literal SEARCH fails to match.
    """
    if "\r\n" not in current or "\n" not in search:
        return None
    candidate = with_crlf(search)
    return candidate if candidate in current else None


def plan(blocks: list[Block], cwd: Path, allowed: set[Path] | None) -> dict[Path, str]:
    """The new content of every touched file; raises before anything is written."""
    contents: dict[Path, str | None] = {}
    for block in blocks:
        target = (cwd / block.path).resolve()
        if allowed is not None and target not in allowed:
            raise PatchError(f"{block.path}: not in this task's files; refusing the whole patch", 2)
        if target not in contents:
            contents[target] = read(target) if target.is_file() else None
        current = contents[target]
        if current is None:
            if block.search:
                raise PatchError(f"{block.path} (block at line {block.line}): file does not exist", 2)
            contents[target] = block.replace
            continue
        count = current.count(block.search) if block.search else 0
        search, replace = block.search, block.replace
        if count != 1 and search:
            variant = crlf_variant(search, current)
            if variant is not None:
                search, replace = variant, with_crlf(replace)
                count = current.count(search)
        if count != 1:
            found = "an empty SEARCH on an existing file" if not block.search else f"{count} matches"
            raise PatchError(
                f"{block.path} (block at line {block.line}): SEARCH must match exactly once, found {found}",
                2,
            )
        contents[target] = current.replace(search, replace, 1)
    return {path: text for path, text in contents.items() if text is not None}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(prog="patch.py", description=__doc__.split("\n")[0])
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("patch", nargs="?", type=Path)
    args = parser.parse_args(argv)
    cwd = Path.cwd()
    try:
        text = args.patch.read_text(encoding="utf-8") if args.patch else sys.stdin.read()
        result = plan(parse(text), cwd, allowed_files(cwd))
    except PatchError as exc:
        print(f"patch.py: {exc}", file=sys.stderr)
        return exc.code
    except OSError as exc:
        print(f"patch.py: {exc}", file=sys.stderr)
        return 1
    for path, content in result.items():
        if not args.dry_run:
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("w", encoding="utf-8", newline="") as out:
                out.write(content)
        print(f"{'would patch' if args.dry_run else 'patched'} {os.path.relpath(path, cwd)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
