"""Materialize `clud`'s argv[0] aliases beside a built or installed `clud`.

`clud` is the only executable clud ships (#1551): `clud-cmd-scan`,
`clud-block-bad-cmd` and `clud-shim` are names it answers to, not files the
build produces. Tests resolve them as siblings of the `clud` under test
(``Path(clud).with_name("clud-cmd-scan")``), so every harness that points at a
fresh build calls :func:`materialize` first. It mirrors the launcher's own
order (`crates/clud-bin/src/alias_link.rs`): hardlink, then symlink, then copy,
and never fails because a link could not be made.
"""

from __future__ import annotations

import os
import shutil
from collections.abc import Mapping
from pathlib import Path

#: The aliases the test suites resolve by name.
TEST_ALIASES = ("clud-cmd-scan", "clud-block-bad-cmd", "clud-shim")


def _is_fresh(source: Path, target: Path) -> bool:
    try:
        if target.is_symlink():
            return target.resolve() == source.resolve()
        if not target.is_file():
            return False
        if os.path.samefile(source, target):
            return True
        a, b = source.stat(), target.stat()
        return a.st_size == b.st_size and a.st_mtime_ns == b.st_mtime_ns
    except OSError:
        return False


def _link(source: Path, target: Path) -> None:
    staging = target.with_name(f".{target.name}.{os.getpid()}.tmp")
    for method in (os.link, os.symlink, shutil.copy2):
        staging.unlink(missing_ok=True)
        try:
            method(source, staging)
            os.replace(staging, target)
            return
        except OSError:
            continue
    staging.unlink(missing_ok=True)
    raise OSError(f"could not link or copy {source} to {target}")


def links_allowed_beside(clud: Path, env: Mapping[str, str]) -> bool:
    """False for a `clud` inside the exact installer candidate artifact (#1718).

    The installer acceptance job points `CLUD_TEST_BINARY` at the candidate
    binary in `CLUD_CANDIDATE_ARTIFACT`, a directory `verify_artifact` requires
    to hold exactly the binary and its provenance. Linking the test aliases
    there would make the artifact fail its own check.
    """
    artifact = env.get("CLUD_CANDIDATE_ARTIFACT")
    if not artifact:
        return True
    try:
        return clud.resolve().parent != Path(artifact).resolve()
    except OSError:
        return True


def materialize(clud: Path, names: tuple[str, ...] = TEST_ALIASES) -> dict[str, Path]:
    """Link every alias in `names` beside `clud`; return name -> path.

    An alias that already resolves to `clud` is left alone, and a stale one
    (after a rebuild replaced `clud`) is replaced. A directory that cannot be
    written yields only the aliases already present.
    """
    clud = clud.resolve()
    suffix = ".exe" if clud.suffix.lower() == ".exe" else ""
    found: dict[str, Path] = {}
    for name in names:
        target = clud.with_name(name + suffix)
        if not _is_fresh(clud, target):
            try:
                _link(clud, target)
            except OSError:
                if not target.is_file():
                    continue
        found[name] = target
    return found
