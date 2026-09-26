"""Detect a test run that writes the checkout's tracked files (#1426).

A full `bash test` once left `.claude/settings.json` and `.codex/hooks.json`
rewritten in the checkout, and the change was later committed by accident
(#1333, #1428). Both suites now snapshot the tracked files before running and
fail if the snapshot differs afterwards: `tests/conftest.py` for pytest and
`ci/test.py` around the Rust suites.

The snapshot is the tracked-file status plus a content hash of every file that
is already dirty, so a checkout that was dirty before the run still passes, and
a test that further edits an already-modified file is still caught.
"""

from __future__ import annotations

import hashlib
from pathlib import Path

from ci import process

Snapshot = dict[str, tuple[str, str | None]]


def _git_status(root: Path) -> str | None:
    try:
        result = process.run(
            [
                "git",
                "--no-optional-locks",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=no",
            ],
            cwd=root,
            capture_output=True,
            text=True,
        )
    except OSError:  # git is not installed
        return None
    if result.returncode != 0:
        return None
    return result.stdout or ""


def _digest(path: Path) -> str | None:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return None


def snapshot(root: Path) -> Snapshot | None:
    """Status and content hash of each dirty tracked file under `root`.

    `None` when `root` is not a git checkout or git is unavailable, so callers
    can skip the check instead of failing.
    """
    output = _git_status(root)
    if output is None:
        return None
    entries = output.split("\0")
    result: Snapshot = {}
    index = 0
    while index < len(entries):
        entry = entries[index]
        index += 1
        if len(entry) < 4:
            continue
        status, path = entry[:2], entry[3:]
        if "R" in status or "C" in status:
            index += 1  # the rename/copy source follows as its own entry
        result[path] = (status, _digest(root / path))
    return result


def changed_paths(before: Snapshot, after: Snapshot) -> list[str]:
    """Tracked paths whose status or content differs between two snapshots."""
    return sorted(
        path for path in before.keys() | after.keys() if before.get(path) != after.get(path)
    )


def describe(paths: list[str], *, during: str) -> str:
    listed = "\n".join(f"  {path}" for path in paths)
    return (
        f"{during} modified tracked files in the checkout (#1426):\n{listed}\n"
        "A test must write only to temp dirs; point the writer at tmp_path or an "
        "isolated repo/HOME. Restore the files with `git checkout -- <path>`."
    )
