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
    """The launching clud, which is multicall (#1551): `clud __cmd-scan`
    runs the native scanner that `clud-cmd-scan` and the legacy
    `clud-block-bad-cmd` names alias."""
    launching_exe = os.environ.get("CLUD_EXE")
    if not launching_exe or not Path(launching_exe).is_absolute():
        return None
    clud = Path(launching_exe)
    return clud if clud.is_file() else None


def main() -> int:
    native = _native_path()
    if native is None:
        print(
            "[block-bad-cmd hook] CLUD_EXE is not a clud executable; "
            "reinstall or upgrade clud.",
            file=sys.stderr,
        )
        return 1
    try:
        completed = RunningProcess.run(
            [str(native), "__cmd-scan"],
            input=sys.stdin.buffer.read(),
            stdout=PIPE,
            stderr=PIPE,
            text=False,
            check=False,
        )
    except FileNotFoundError:
        print(
            "[block-bad-cmd hook] native helper disappeared; "
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
