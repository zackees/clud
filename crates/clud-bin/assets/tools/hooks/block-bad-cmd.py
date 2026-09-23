#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""Compatibility shim for the native PreToolUse command guard.

The hot path is the sibling Rust executable `clud-cmd-scan`.
This Python file remains managed for one release so existing hand-written
hook configs that still invoke `"$CLUD_EXE" tool run hooks/block-bad-cmd.py`
continue to work. New hook wiring invokes the sibling `clud-cmd-scan`
directly to avoid launching Python or uv.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

from running_process import PIPE, RunningProcess


def _native_path() -> Path | None:
    launching_exe = os.environ.get("CLUD_EXE")
    if not launching_exe or not Path(launching_exe).is_absolute():
        return None
    sibling = Path(launching_exe).with_name(
        "clud-cmd-scan.exe" if os.name == "nt" else "clud-cmd-scan"
    )
    if sibling.is_file():
        return sibling
    old_sibling = Path(launching_exe).with_name(
        "clud-block-bad-cmd.exe" if os.name == "nt" else "clud-block-bad-cmd"
    )
    return old_sibling if old_sibling.is_file() else None


def main() -> int:
    native = _native_path()
    if native is None:
        print(
            "[block-bad-cmd hook] no native helper beside CLUD_EXE; "
            "reinstall or upgrade clud.",
            file=sys.stderr,
        )
        return 1
    try:
        completed = RunningProcess.run(
            [str(native)],
            input=sys.stdin.buffer.read(),
            stdout=PIPE,
            stderr=PIPE,
            text=False,
            check=False,
        )
    except FileNotFoundError:
        print(
            "[block-bad-cmd hook] sibling native helper disappeared; "
            "reinstall or upgrade clud.",
            file=sys.stderr,
        )
        return 1
    if completed.stdout:
        sys.stdout.buffer.write(completed.stdout)
        sys.stdout.buffer.flush()
    if completed.stderr:
        sys.stderr.buffer.write(completed.stderr)
        sys.stderr.buffer.flush()
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
