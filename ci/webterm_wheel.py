"""Bundle the separately-built web terminal next to clud in desktop wheels."""

from __future__ import annotations

import zipfile
from pathlib import Path

from ci.wheel_rewrite import new_entry, rewrite_wheel, wheel_dist_info_dir


def desktop_target(target: str) -> bool:
    return "windows" in target or "apple-darwin" in target


def companion_name(target: str) -> str:
    return "clud-webterm.exe" if "windows" in target else "clud-webterm"


def add_companion(wheel: Path, companion: Path, target: str) -> None:
    """Atomically add a companion script and regenerate the wheel RECORD."""
    if not companion.is_file():
        raise RuntimeError(f"web terminal binary is missing: {companion}")
    dist_info = wheel_dist_info_dir(wheel)
    script = f"{dist_info.removesuffix('.dist-info')}.data/scripts/{companion_name(target)}"
    # A same-named old companion is replaced by the added entry.
    rewrite_wheel(wheel, add=[new_entry(script, companion.read_bytes())])


def wheel_has_companion(wheel: Path, target: str) -> bool:
    needle = f".data/scripts/{companion_name(target)}"
    with zipfile.ZipFile(wheel) as archive:
        return any(name.endswith(needle) for name in archive.namelist())
