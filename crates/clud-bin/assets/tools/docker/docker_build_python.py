#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""docker_build_python.py — uv-managed Python docker-build stack.

**v0 scope: `init` only.** The other subcommands (up / run / shell /
verify / clean / doctor) exit 64 (EX_USAGE) with a notice; they need
the same harness wiring as the soldr stack plus a representative uv
project to verify against. Tracked in zackees/clud#421's
"NOT IN THIS ISSUE" section.

The volume contract this stack will follow once fleshed out:

  anon volumes:
    <proj>-venv         /work/.venv      (DO NOT bind-mount — symlinks)
    <proj>-uv-cache     /root/.cache/uv
    <proj>-pip-cache    /root/.cache/pip
  bind:
    <repo>:/work:ro

The `.venv` symlink trap is the reason this stack exists separately
from cpp's harness — on Windows hosts NTFS does not translate POSIX
symlinks across the FS layer; the anon volume sidesteps that.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

STACK = "python"

DOCKERFILE = r"""# managed-by: clud (docker_build_python.py)
# uv-first Python stack. uv installs are restartable, and the
# in-container venv lives in an anon volume so symlinks survive
# across runs without translating through the host FS.
FROM python:3.11-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        bash ca-certificates curl git build-essential \
 && rm -rf /var/lib/apt/lists/* \
 && pip install --no-cache-dir uv

ENV UV_CACHE_DIR=/root/.cache/uv \
    VIRTUAL_ENV=/work/.venv \
    PATH=/work/.venv/bin:$PATH

RUN mkdir -p /work /root/.cache/uv /root/.cache/pip
WORKDIR /work
CMD ["bash", "-l"]
"""

ENTRY_SH = r"""#!/usr/bin/env bash
# managed-by: clud (docker_build_python.py)
set -euo pipefail
exec tail -f /dev/null
"""

STACK_TOML = r"""# managed-by: clud (docker_build_python.py)
[stack]
name = "python"
image_tag_base = "clud-docker-build-python"

[volumes]
venv = "/work/.venv"
uv_cache = "/root/.cache/uv"
pip_cache = "/root/.cache/pip"

[env]
UV_CACHE_DIR = "/root/.cache/uv"
VIRTUAL_ENV = "/work/.venv"
"""

NOT_IMPLEMENTED = (
    "docker_build_python: {sub} is not implemented in v0 — "
    "follow-up tracked in zackees/clud#421 ('NOT IN THIS ISSUE').\n"
)


def cmd_init(path: Path) -> int:
    out = path / ".clud" / "docker-build" / STACK
    out.mkdir(parents=True, exist_ok=True)
    (out / "Dockerfile").write_text(DOCKERFILE, encoding="utf-8")
    entry = out / "entry.sh"
    entry.write_text(ENTRY_SH, encoding="utf-8")
    try:
        os.chmod(entry, 0o755)
    except (OSError, NotImplementedError):
        pass
    (out / "stack.toml").write_text(STACK_TOML, encoding="utf-8")
    sys.stdout.write(f"wrote {out}/{{Dockerfile,entry.sh,stack.toml}}\n")
    return 0


def main(argv: list[str]) -> int:
    if not argv:
        sys.stderr.write("usage: docker_build_python.py <path> <subcommand>\n")
        return 2

    path_arg = argv[0]
    sub = argv[1] if len(argv) > 1 else "verify"

    if path_arg == "doctor":
        # python stack doctor is a TODO — exit 0 so the trampoline's
        # cross-stack doctor sweep does not falsely fail when no python
        # check has been authored yet.
        sys.stdout.write("doctor (python): no checks yet — see #421\n")
        return 0

    path = Path(path_arg).resolve()

    if sub == "init":
        return cmd_init(path)

    sys.stderr.write(NOT_IMPLEMENTED.format(sub=sub))
    return 64


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
