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


def _cargo(subcommand: list[str]) -> list[str]:
    """Return the cargo argv, preferring `soldr cargo` on Windows (issue #27)."""
    from ci.env import cargo_argv, clean_env

    return cargo_argv(subcommand, env=clean_env())


def main(argv: list[str] | None = None) -> int:
    from ci.env import activate

    activate()

    argv = list(sys.argv[1:] if argv is None else argv)
    run_integration = "--integration" in argv or "--full" in argv
    pytest_args = [a for a in argv if a not in ("--integration", "--full")]

    # Rust tests
    if run(_cargo(["test", "--workspace", "--no-run"])) != 0:
        return 1
    cargo_test = _cargo(["test", "--workspace"])
    if sys.platform == "win32":
        cargo_test += ["--", "--test-threads=1"]
    if run(cargo_test) != 0:
        return 1

    # Python unit tests (skip integration by default)
    pytest_cmd = [sys.executable, "-m", "pytest", "-m", "not integration", *pytest_args]
    if not _pytest_ok(run(pytest_cmd)):
        return 1

    # Integration tests (only when requested). `-v` prints each test name
    # before it runs so a hang in CI is pinned to the exact test rather than
    # appearing as silent dead air.
    if run_integration:
        from ci.env import clean_env

        env = clean_env()
        env["CLUD_INTEGRATION_TESTS"] = "1"
        # Disable the Windows exe-unlock dance for every clud subprocess
        # spawned by tests. See #37: the rename+copy+GC pattern appears to
        # keep stdout/stderr pipe handles alive on Windows CI, which wedges
        # subprocess.run in a pipe-EOF wait. Tests don't need hot-reload
        # protection, so this is strictly safer for the test harness.
        env["CLUD_NO_UNLOCK"] = "1"
        int_cmd = [
            sys.executable,
            "-m",
            "pytest",
            "-m",
            "integration",
            "-v",
            *pytest_args,
        ]
        rc = subprocess.run(int_cmd, cwd=ROOT, env=env).returncode
        if not _pytest_ok(rc):
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
