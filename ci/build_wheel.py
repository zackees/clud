"""Build clud Rust binary and package as a Python wheel via maturin."""

from __future__ import annotations

import argparse
import contextlib
import platform
import subprocess
import sys
from pathlib import Path
from typing import Literal

from ci.wheel_repair import repair_windows_gnu_wheel

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"

BuildMode = Literal["dev", "release"]


def build_command(mode: BuildMode, env: dict[str, str] | None = None) -> list[str]:
    from ci.env import maturin_argv

    subcommand = [
        "build",
        "--interpreter",
        sys.executable,
        "--out",
        str(DIST),
    ]
    if mode == "dev":
        subcommand.extend(["--profile", "dev"])
    else:
        subcommand.append("--release")
        if platform.system() == "Linux":
            subcommand.extend(["--zig", "--compatibility", "manylinux2014"])
        else:
            subcommand.extend(["--compatibility", "pypi"])
    # Use the dev-venv maturin via `python -m maturin`. The MSVC rustup-toolchain
    # pin from issue #27 is preserved by `_windows_build_env` exporting CARGO/
    # RUSTC/RUSTUP_TOOLCHAIN; routing maturin itself through soldr fails on
    # Linux because PyO3/maturin only publishes musl Linux release assets.
    return maturin_argv(subcommand, env=env)


def built_wheels() -> list[Path]:
    return sorted(DIST.glob("clud-*.whl"), key=lambda path: path.stat().st_mtime)


def latest_wheel() -> Path:
    wheels = built_wheels()
    if not wheels:
        raise RuntimeError(f"no built wheel found in {DIST}")
    return wheels[-1]


def install_wheel(wheel: Path, *, env: dict[str, str]) -> int:
    install = subprocess.run(
        [
            "uv",
            "pip",
            "install",
            "--python",
            sys.executable,
            "--reinstall",
            "--no-deps",
            str(wheel),
        ],
        cwd=ROOT,
        check=False,
        env=env,
    )
    if install.returncode != 0:
        return install.returncode

    for pth in (ROOT / ".venv").glob("**/site-packages/clud.pth"):
        with contextlib.suppress(OSError):
            pth.unlink()
    return 0


def run_build(mode: BuildMode) -> int:
    from ci.env import build_env

    env = build_env()
    DIST.mkdir(parents=True, exist_ok=True)
    before = {path.name for path in built_wheels()}
    cmd = build_command(mode, env=env)
    print(f"build mode: {mode}", file=sys.stderr, flush=True)
    result = subprocess.run(cmd, cwd=ROOT, check=False, env=env)
    if result.returncode != 0:
        return result.returncode
    for wheel in built_wheels():
        repair_windows_gnu_wheel(wheel)
    if mode != "dev":
        return 0

    wheel = latest_wheel()
    action = "reinstalling existing dev wheel" if wheel.name in before else "installing dev wheel"
    print(f"{action}: {wheel.name}", file=sys.stderr, flush=True)
    return install_wheel(wheel, env=env)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build clud")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--dev", action="store_true", help="build dev-profile wheel and reinstall")
    mode.add_argument("--release", action="store_true", help="build release wheel(s) into dist/")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None, *, default_mode: BuildMode = "release") -> int:
    args = parse_args(argv)
    mode: BuildMode = default_mode
    if args.dev:
        mode = "dev"
    if args.release:
        mode = "release"
    return run_build(mode)


if __name__ == "__main__":
    sys.exit(main())
