"""Lint orchestrator for clud: cargo fmt + clippy + ruff + banned imports."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run(cmd: list[str]) -> int:
    from ci.env import clean_env

    return subprocess.run(cmd, cwd=ROOT, env=clean_env()).returncode


def _cargo(subcommand: list[str]) -> list[str]:
    """Return the cargo argv, preferring `soldr cargo` on Windows (issue #27)."""
    from ci.env import cargo_argv, clean_env

    return cargo_argv(subcommand, env=clean_env())


def main() -> int:
    from ci.env import activate

    activate()

    from ci.banned_imports import main as check_banned_imports

    if check_banned_imports() != 0:
        return 1
    if run(_cargo(["fmt", "--all", "--check"])) != 0:
        return 1
    if run(_cargo(["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])) != 0:
        return 1
    if run([sys.executable, "-m", "ruff", "check", "src", "tests", "ci"]) != 0:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
