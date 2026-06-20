#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""docker-build.py — trampoline for the docker-build tool family.

Dispatches to a per-stack tool (`docker_build_soldr.py`, `docker_build_python.py`,
`docker_build_cpp.py`) based on the first positional argument. In-process
dispatch via `importlib.util` so the trampoline is sugar — invoking
`clud tool run docker/docker-build.py soldr <path> init` and
`clud tool run docker/docker_build_soldr.py <path> init` produce literally
identical behavior (same uv venv, same module objects, no subprocess fork).

Invocation:

    clud tool run docker/docker-build.py <stack> <path> [subcommand]
    clud tool run docker/docker-build.py doctor

`<stack>` is one of `soldr` / `python` / `cpp`. The special first arg `doctor`
runs the cross-stack diagnostic (docker daemon up, clock skew, MSYS path
mangling, Docker Desktop `clock=host`) by dispatching to each stack's doctor.

Exit codes:
  0   success / all green
  2   usage error (missing or unknown stack name)
  64  EX_USAGE — propagated from per-stack tool when it sees a bad subcommand
  *   propagated verbatim from the per-stack tool otherwise

See `crates/clud-bin/assets/tools/docker/README.md` for the full shape table
and the cross-references to the architectural design (zackees/clud#416,
implementation issue zackees/clud#421).
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

STACKS = ("soldr", "python", "cpp")

USAGE = """\
usage: clud tool run docker/docker-build.py <stack> <path> [subcommand]
       clud tool run docker/docker-build.py doctor

Stacks: soldr (Rust + soldr + zccache), python (uv-managed), cpp (CMake + ccache)
Subcommands: init | up | run -- <cmd...> | shell | verify | clean | doctor
"""


def _load_sibling(name: str):
    here = Path(__file__).resolve().parent
    src = here / f"docker_build_{name}.py"
    if not src.is_file():
        sys.stderr.write(f"docker-build: missing sibling tool: {src}\n")
        sys.exit(2)
    spec = importlib.util.spec_from_file_location(f"docker_build_{name}", src)
    if spec is None or spec.loader is None:
        sys.stderr.write(f"docker-build: cannot load spec for {src}\n")
        sys.exit(2)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def main(argv: list[str]) -> int:
    if not argv:
        sys.stderr.write(USAGE)
        return 2
    first = argv[0]

    if first in ("-h", "--help", "help"):
        sys.stdout.write(USAGE)
        return 0

    if first == "doctor":
        # Aggregate doctor across all stacks. First nonzero wins.
        worst = 0
        for stack in STACKS:
            mod = _load_sibling(stack)
            sys.stdout.write(f"\n=== doctor: {stack} ===\n")
            rc = mod.main(["doctor"])
            worst = worst or rc
        return worst

    if first not in STACKS:
        sys.stderr.write(f"docker-build: unknown stack `{first}`. "
                         f"Expected one of: {', '.join(STACKS)} or `doctor`.\n")
        sys.stderr.write(USAGE)
        return 2

    mod = _load_sibling(first)
    return mod.main(argv[1:])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
