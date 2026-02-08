"""Lint checker that verifies _version.py and pyproject.toml versions are in sync.

Usage:
    python -m clud.lint.version_checker
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


def _find_project_root() -> Path:
    """Find the project root by looking for pyproject.toml."""
    current = Path(__file__).resolve()
    for parent in current.parents:
        if (parent / "pyproject.toml").exists():
            return parent
    msg = "Could not find pyproject.toml in any parent directory"
    raise FileNotFoundError(msg)


def _read_version_from_version_file(root: Path) -> str:
    """Read the version from _version.py.

    Args:
        root: Project root directory

    Returns:
        Version string from _version.py
    """
    version_file = root / "src" / "clud" / "_version.py"
    if not version_file.exists():
        msg = f"{version_file} does not exist"
        raise FileNotFoundError(msg)

    content = version_file.read_text(encoding="utf-8")
    match = re.search(r'__version__\s*=\s*["\']([^"\']+)["\']', content)
    if not match:
        msg = f"Could not find __version__ in {version_file}"
        raise ValueError(msg)

    return match.group(1)


def _read_version_from_pyproject(root: Path) -> str:
    """Read the version from pyproject.toml.

    Args:
        root: Project root directory

    Returns:
        Version string from pyproject.toml
    """
    pyproject_file = root / "pyproject.toml"
    content = pyproject_file.read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', content, re.MULTILINE)
    if not match:
        msg = f"Could not find version in {pyproject_file}"
        raise ValueError(msg)

    return match.group(1)


def check_version_sync() -> int:
    """Check that _version.py and pyproject.toml versions match.

    Returns:
        0 if versions match, 1 if they don't
    """
    root = _find_project_root()

    version_py = _read_version_from_version_file(root)
    pyproject_version = _read_version_from_pyproject(root)

    if version_py != pyproject_version:
        print("Version mismatch!")
        print(f"  src/clud/_version.py:  {version_py}")
        print(f"  pyproject.toml:        {pyproject_version}")
        print()
        print("Update both files to the same version.")
        return 1

    print(f"Version sync OK: {version_py}")
    return 0


def main() -> int:
    """Main entry point.

    Returns:
        Exit code
    """
    return check_version_sync()


if __name__ == "__main__":
    sys.exit(main())
