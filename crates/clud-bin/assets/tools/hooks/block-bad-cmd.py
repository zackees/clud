#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""Compatibility shim for the native PreToolUse command guard.

The hot path is the PyPI-shipped Rust executable `clud-block-bad-cmd`.
This Python file remains managed for one release so existing hand-written
hook configs that still invoke `clud tool run hooks/block-bad-cmd.py`
continue to work. New hook wiring should invoke `clud-block-bad-cmd`
directly to avoid launching Python or uv.
"""

from __future__ import annotations

import os
import sys

from running_process import PIPE, RunningProcess


def _native_name() -> str:
    return "clud-block-bad-cmd.exe" if os.name == "nt" else "clud-block-bad-cmd"


def main() -> int:
    try:
        completed = RunningProcess.run(
            [_native_name()],
            input=sys.stdin.buffer.read(),
            stdout=PIPE,
            stderr=PIPE,
            text=False,
            check=False,
        )
    except FileNotFoundError:
        print(
            "[block-bad-cmd hook] clud-block-bad-cmd not found on PATH; "
            "allowing command for compatibility. Reinstall or upgrade clud.",
            file=sys.stderr,
        )
        return 0
    if completed.stdout:
        sys.stdout.buffer.write(completed.stdout)
        sys.stdout.buffer.flush()
    if completed.stderr:
        sys.stderr.buffer.write(completed.stderr)
        sys.stderr.buffer.flush()
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
