"""Materialize argv[0] aliases of the multicall `clud` binary (#1551).

clud ships one executable. `clud-cmd-scan`, `clud-block-bad-cmd` and
`clud-shim` are names that same binary answers to (see
`crates/clud-bin/src/multicall.rs`). A running clud creates its own aliases
under `~/.clud/state/`; build trees and test bundles have no such launch, so
tests and CI steps that invoke a helper by path call `materialize` first to
put hardlinks (copies where a link fails) beside the built `clud`.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

#: Helper names tests and CI invoke by path beside the built `clud`.
ALIASES = ("clud-shim", "clud-cmd-scan", "clud-block-bad-cmd")


def _exe(name: str) -> str:
    return name + ".exe" if os.name == "nt" else name


def _current(source: Path, target: Path) -> bool:
    try:
        src = source.stat()
        dst = target.stat()
    except OSError:
        return False
    if src.st_size != dst.st_size:
        return False
    if (src.st_dev, src.st_ino) == (dst.st_dev, dst.st_ino):
        return True
    return dst.st_mtime >= src.st_mtime


def materialize(clud: Path, names: tuple[str, ...] = ALIASES) -> dict[str, Path]:
    """Place each alias beside `clud` (hardlink, else copy); return name -> path.

    An alias already current for this `clud` is left alone, so concurrent
    callers and repeated test sessions are cheap.
    """
    clud = Path(clud)
    placed: dict[str, Path] = {}
    if not clud.is_file():
        return placed
    for name in names:
        target = clud.with_name(_exe(name))
        placed[name] = target
        if _current(clud, target):
            continue
        staging = target.with_name(f".{target.name}.{os.getpid()}.tmp")
        try:
            os.link(clud, staging)
        except OSError:
            shutil.copy2(clud, staging)
            staging.chmod(0o755)
        os.replace(staging, target)
    return placed


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: python -m ci.multicall_aliases <path-to-clud>", file=sys.stderr)
        return 2
    clud = Path(argv[1])
    if not clud.is_file():
        print(f"not a file: {clud}", file=sys.stderr)
        return 1
    for name, path in materialize(clud).items():
        print(f"{name} -> {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
