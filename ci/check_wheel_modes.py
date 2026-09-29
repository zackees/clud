"""Fail when a non-Windows wheel ships a non-executable script (#1544, #1545).

pip installs each `<dist>.data/scripts/*` entry with the Unix mode stored in
its zip `external_attr`. 2.8.14 and 2.8.20 stored 0644, so `pip install clud`
produced a `clud` that could not run. This checks the stored mode before a
wheel leaves the build job.

Deliberately stdlib-only, with no `ci.*` imports, so other maturin
Rust+Python projects can copy it verbatim and add one workflow step:

    python -m ci.check_wheel_modes --dist-dir dist/
"""

from __future__ import annotations

import argparse
import stat
import sys
import zipfile
from pathlib import Path


def _is_windows_wheel(wheel: Path) -> bool:
    platform_tag = wheel.stem.rsplit("-", 1)[-1].lower()
    return any(tag.startswith("win") for tag in platform_tag.split("."))


def check_wheel(wheel: Path) -> list[str]:
    """Return one message per script entry that pip would install unusable."""
    if _is_windows_wheel(wheel):
        return []
    errors: list[str] = []
    with zipfile.ZipFile(wheel) as archive:
        for info in archive.infolist():
            parts = info.filename.split("/")
            if len(parts) < 3 or not parts[0].endswith(".data") or parts[1] != "scripts":
                continue
            if info.is_dir():
                continue
            mode = info.external_attr >> 16
            if not stat.S_ISREG(mode):
                problem = "is not a regular file"
            elif mode & 0o111 == 0:
                problem = "is not executable"
            else:
                continue
            errors.append(f"{wheel.name}: {info.filename} {problem} (mode {mode:#o})")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dist-dir", type=Path, default=Path("dist"))
    args = parser.parse_args(argv)
    wheels = sorted(args.dist_dir.glob("*.whl"))
    if not wheels:
        print(f"no wheels found in {args.dist_dir}", file=sys.stderr)
        return 1
    errors: list[str] = []
    for wheel in wheels:
        found = check_wheel(wheel)
        status = "skipped (Windows)" if _is_windows_wheel(wheel) else ("FAIL" if found else "ok")
        print(f"{wheel.name}: script modes {status}")
        errors += found
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
