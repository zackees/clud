#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
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
import subprocess
import sys


def _native_name() -> str:
    return "clud-block-bad-cmd.exe" if os.name == "nt" else "clud-block-bad-cmd"


def main() -> int:
    try:
        completed = subprocess.run(
            [_native_name()],
            stdin=sys.stdin.buffer,
            stdout=sys.stdout.buffer,
            stderr=sys.stderr.buffer,
            check=False,
        )
    except FileNotFoundError:
        print(
            "[block-bad-cmd hook] clud-block-bad-cmd not found on PATH; "
            "allowing command for compatibility. Reinstall or upgrade clud.",
            file=sys.stderr,
        )
        return 0
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
