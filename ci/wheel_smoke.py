"""Install-smoke the wheel that ships, on a native exec runner (#1545).

The PR lane installs the bundle's *dev* wheel into the repo venv
(`ci/run_bundle.py::install_wheel`). The release wheel went through extra
rewrites (the ELF strip) and was never installed anywhere, which is how
2.8.14/2.8.20 shipped a non-executable `clud` (#1544). This creates a fresh
venv, installs the downloaded release wheel with that venv's real `pip`, and
runs `ci.build_wheel.verify_installed_scripts` against its scripts dir.

    python -m ci.wheel_smoke --wheel-dir release-wheel
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile
from pathlib import Path

from ci import process


def select_wheel(wheel_dir: Path) -> Path:
    wheels = sorted(wheel_dir.glob("*.whl"))
    if len(wheels) != 1:
        names = ", ".join(wheel.name for wheel in wheels) or "none"
        raise SystemExit(f"expected exactly one wheel in {wheel_dir}, found: {names}")
    return wheels[0]


def venv_scripts_dir(venv: Path) -> Path:
    return venv / ("Scripts" if os.name == "nt" else "bin")


def smoke(wheel: Path) -> int:
    from ci.build_wheel import verify_installed_scripts

    with tempfile.TemporaryDirectory(prefix="clud-wheel-smoke-") as temp_dir:
        venv = Path(temp_dir) / "venv"
        for argv in (
            [sys.executable, "-m", "venv", str(venv)],
            [
                str(venv_scripts_dir(venv) / ("python.exe" if os.name == "nt" else "python")),
                "-m",
                "pip",
                "install",
                "--no-deps",
                "--no-index",
                "--disable-pip-version-check",
                str(wheel),
            ],
        ):
            print(f"+ {' '.join(argv)}", flush=True)
            if process.run(argv, check=False).returncode != 0:
                return 1
        rc = verify_installed_scripts(env=dict(os.environ), scripts_dir=venv_scripts_dir(venv))
    print(f"release wheel smoke {'passed' if rc == 0 else 'FAILED'}: {wheel.name}", flush=True)
    return rc


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--wheel-dir", type=Path, required=True)
    args = parser.parse_args(argv)
    return smoke(select_wheel(args.wheel_dir))


if __name__ == "__main__":
    sys.exit(main())
