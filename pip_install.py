"""Install/upgrade clud on Windows by renaming the locked .exe first.

Usage:
    python pip_install.py                  # pip install -e .
    python pip_install.py --upgrade        # pip install --upgrade clud
    python pip_install.py -- -e .[dev]     # pip install -e .[dev]

On Windows, a running clud.exe cannot be deleted. This script renames it
to ~/.clud/garbage/clud_tmpNNN.exe before running pip install, so the
installer can write the new exe to the original path. The renamed file
will be cleaned up automatically next time clud launches.
"""

import shutil
import subprocess
import sys
from pathlib import Path


def find_clud_exe() -> Path | None:
    """Find the clud.exe in the current environment's Scripts directory."""

    # Check the current venv/environment Scripts dir
    scripts_dir = Path(sys.prefix) / "Scripts"
    exe_path = scripts_dir / "clud.exe"
    if exe_path.exists():
        return exe_path

    # Also check via shutil.which
    which_result = shutil.which("clud")
    if which_result:
        p = Path(which_result)
        if p.suffix.lower() == ".exe" and p.exists():
            return p

    return None


def get_garbage_dir() -> Path:
    """Get or create the ~/.clud/garbage directory."""
    garbage_dir = Path.home() / ".clud" / "garbage"
    garbage_dir.mkdir(parents=True, exist_ok=True)
    return garbage_dir


def next_garbage_name(garbage_dir: Path) -> Path:
    """Find the next available clud_tmpNNN.exe name."""
    for i in range(1, 10000):
        candidate = garbage_dir / f"clud_tmp{i:03d}.exe"
        if not candidate.exists():
            return candidate
    msg = "Too many garbage files in ~/.clud/garbage/ — clean them up first"
    raise RuntimeError(msg)


def stash_exe(exe_path: Path) -> Path | None:
    """Rename a locked clud.exe into the garbage directory.

    Returns the new path on success, None if the file wasn't locked or
    didn't need stashing.
    """
    if not exe_path.exists():
        return None

    garbage_dir = get_garbage_dir()
    dest = next_garbage_name(garbage_dir)

    try:
        exe_path.rename(dest)
        print(f"Renamed {exe_path} -> {dest}")
        return dest
    except OSError as e:
        print(f"Warning: Could not rename {exe_path}: {e}")
        return None


def main() -> int:
    """Stash any running clud.exe and run pip install."""
    # Collect pip arguments: everything after '--', or default to '-e .'
    if "--" in sys.argv:
        idx = sys.argv.index("--")
        pip_args = sys.argv[idx + 1 :]
    elif "--upgrade" in sys.argv:
        pip_args = ["--upgrade", "clud"]
    else:
        pip_args = ["-e", "."]

    # On Windows, try to stash the existing exe
    if sys.platform == "win32":
        exe_path = find_clud_exe()
        if exe_path:
            stash_exe(exe_path)

    # Run pip install
    cmd = [sys.executable, "-m", "pip", "install", *pip_args]
    print(f"Running: {' '.join(cmd)}")
    result = subprocess.run(cmd)
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
