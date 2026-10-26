#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "ast-grep-cli==0.51.0",
#   "running-process==4.10.1",
# ]
# ///
# managed-by: clud
"""ast_grep.py — structural search and rewrite with a pinned ast-grep (#1339 item 6).

Usage:
  ast_grep.py [run|scan] <ast-grep arguments>

Runs the `ast-grep` binary from the pinned `ast-grep-cli` wheel, so no
system install is needed on any platform. Arguments pass through unchanged:

  "$CLUD_EXE" tool run code/ast_grep.py run -p 'foo($A)' -l rust src/
  "$CLUD_EXE" tool run code/ast_grep.py run -p 'foo($A)' -r 'bar($A)' -l rust src/

`-r` alone previews a rewrite as a diff; `-U` applies it. In a /grind run
only the integrator may apply (`-U`, `--update-all`, `-i`, `new`); every
other role searches and previews (clud's hook enforces this).

Exit codes are ast-grep's own: 0 when something matched, 1 when nothing did.
127 means the ast-grep binary could not be found in this script's
environment.
"""

from __future__ import annotations

import os
import sys
import sysconfig
from pathlib import Path

from running_process import RunningProcess


def binary() -> Path | None:
    name = "ast-grep.exe" if os.name == "nt" else "ast-grep"
    for directory in (sysconfig.get_path("scripts"), str(Path(sys.executable).parent)):
        candidate = Path(directory) / name
        if candidate.is_file():
            return candidate
    return None


def main(argv: list[str]) -> int:
    exe = binary()
    if exe is None:
        print("ast_grep.py: the ast-grep binary is missing from this environment", file=sys.stderr)
        return 127
    proc = RunningProcess([str(exe), *argv], check=False)
    for line in proc.line_iter(timeout=None):
        print(line, flush=True)
    return proc.wait()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
