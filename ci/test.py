"""Test orchestrator for clud: cargo test + pytest."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# pytest exit code 5 = no tests collected (all deselected by marker).
# This is acceptable when no integration tests exist yet.
_PYTEST_NO_TESTS_COLLECTED = 5


def _pytest_ok(returncode: int) -> bool:
    return returncode in (0, _PYTEST_NO_TESTS_COLLECTED)


def run(cmd: list[str]) -> int:
    from ci.env import clean_env

    return subprocess.run(cmd, cwd=ROOT, env=clean_env()).returncode


def main(argv: list[str] | None = None) -> int:
    from ci.env import activate

    activate()

    argv = list(sys.argv[1:] if argv is None else argv)
    run_integration = "--integration" in argv or "--full" in argv
    pytest_args = [a for a in argv if a not in ("--integration", "--full")]

    # Rust tests
    if run(["cargo", "test", "--workspace", "--no-run"]) != 0:
        return 1
    cargo_test = ["cargo", "test", "--workspace"]
    if sys.platform == "win32":
        cargo_test += ["--", "--test-threads=1"]
    if run(cargo_test) != 0:
        return 1

    # Python unit tests (skip integration by default)
    pytest_cmd = [sys.executable, "-m", "pytest", "-m", "not integration", *pytest_args]
    if not _pytest_ok(run(pytest_cmd)):
        return 1

    # Integration tests (only when requested)
    if run_integration:
        from ci.env import clean_env

        env = clean_env()
        env["CLUD_INTEGRATION_TESTS"] = "1"
        int_cmd = [sys.executable, "-m", "pytest", "-m", "integration", *pytest_args]
        rc = subprocess.run(int_cmd, cwd=ROOT, env=env).returncode
        if not _pytest_ok(rc):
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
