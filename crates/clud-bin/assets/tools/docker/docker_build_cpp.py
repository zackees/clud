#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# managed-by: clud
"""docker_build_cpp.py — CMake + ccache docker-build stack.

**v0 scope: `init` only.** Same status notice as docker_build_python.py;
other subcommands return 64 (EX_USAGE) pending follow-up.

The volume contract this stack will follow once fleshed out:

  anon volumes:
    <proj>-build      /build            (out-of-source CMake build dir)
    <proj>-ccache     /ccache           (CCACHE_DIR)
    <proj>-conan      /root/.conan2     (Conan, if used)
  bind:
    <repo>:/src:ro

`CCACHE_BASEDIR=/src` is critical — strips the absolute-path prefix
from ccache's content hash so the cache is reusable across `$repo`
paths on different developer machines.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

STACK = "cpp"

DOCKERFILE = r"""# managed-by: clud (docker_build_cpp.py)
# CMake + ccache stack. ccache deduplicates compiles across warm runs
# inside the anon volume; CCACHE_BASEDIR rewrites paths so the cache
# stays portable across host-side checkout locations.
FROM gcc:13-bookworm

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        bash ca-certificates curl git cmake ninja-build ccache \
        clang lld pkg-config \
 && rm -rf /var/lib/apt/lists/*

ENV CCACHE_DIR=/ccache \
    CCACHE_BASEDIR=/src \
    PATH=/usr/lib/ccache:$PATH

RUN mkdir -p /build /ccache /src
WORKDIR /src
CMD ["bash", "-l"]
"""

ENTRY_SH = r"""#!/usr/bin/env bash
# managed-by: clud (docker_build_cpp.py)
set -euo pipefail
exec tail -f /dev/null
"""

STACK_TOML = r"""# managed-by: clud (docker_build_cpp.py)
[stack]
name = "cpp"
image_tag_base = "clud-docker-build-cpp"

[volumes]
build = "/build"
ccache = "/ccache"
conan = "/root/.conan2"

[env]
CCACHE_DIR = "/ccache"
CCACHE_BASEDIR = "/src"
"""

NOT_IMPLEMENTED = (
    "docker_build_cpp: {sub} is not implemented in v0 — "
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
        sys.stderr.write("usage: docker_build_cpp.py <path> <subcommand>\n")
        return 2

    path_arg = argv[0]
    sub = argv[1] if len(argv) > 1 else "verify"

    if path_arg == "doctor":
        sys.stdout.write("doctor (cpp): no checks yet — see #421\n")
        return 0

    path = Path(path_arg).resolve()

    if sub == "init":
        return cmd_init(path)

    sys.stderr.write(NOT_IMPLEMENTED.format(sub=sub))
    return 64


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
