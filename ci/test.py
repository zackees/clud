"""Test orchestrator for clud: cargo test + pytest.

Default mode runs unit coverage. `--integration` runs only integration tests;
`--full` runs both.

Every mode builds clud from the working tree and prints which binary the
pytest lanes will run. Pass `--use-installed-clud` to test the wheel already
in `.venv/Scripts` instead — useful for verifying a release artifact, and
never the default, because a silent swap makes source changes vanish from a
run that still reports pass/fail (issue #726).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# pytest exit code 5 = no tests collected (all deselected by marker).
# This is acceptable when no integration tests exist yet.
_PYTEST_NO_TESTS_COLLECTED = 5


def _select_suites(argv: list[str]) -> tuple[bool, bool, list[str]]:
    full = "--full" in argv
    integration = "--integration" in argv or full
    unit = not integration or full
    pytest_args = [a for a in argv if a not in ("--integration", "--full", "--use-installed-clud")]
    return unit, integration, pytest_args


def _use_installed_clud(argv: list[str]) -> bool:
    """Should the integration lane run the *installed* wheel instead of a build?

    Issue #726. This used to be implicit — `--integration` without `--unit`
    silently ran `.venv/Scripts/clud.exe` and skipped building clud entirely,
    so local source changes were absent from the run with no warning. That is
    the worst shape for a testing footgun: the suite still builds something,
    still reports pass/fail, and "my change did nothing" is indistinguishable
    from a real negative result.

    The behaviour came from #161 ("Speed up CI with setup-soldr shims"), which
    wired it into the per-platform `*-integration-test.yml` workflows. Those
    workflows no longer exist — #611 replaced them, and CI now drives pytest
    through `ci/run_bundle.py`, which never imports this module. So the
    optimisation has had no CI consumer for some time and only ever affected
    developers, for whom it is the wrong default.

    Verifying an installed wheel is still a legitimate thing to want, so it
    stays available — just spelled out rather than inferred.
    """
    return "--use-installed-clud" in argv


def _pytest_ok(returncode: int) -> bool:
    return returncode in (0, _PYTEST_NO_TESTS_COLLECTED)


def run(cmd: list[str], *, env: dict[str, str] | None = None) -> int:
    from ci.env import clean_env

    return subprocess.run(cmd, cwd=ROOT, env=env if env is not None else clean_env()).returncode


def _cargo(subcommand: list[str], *, env: dict[str, str] | None = None) -> list[str]:
    """Return the cargo argv for the configured CI environment."""
    from ci.env import cargo_argv, clean_env

    return cargo_argv(subcommand, env=env if env is not None else clean_env())


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _target_debug_dirs(env: dict[str, str]) -> list[Path]:
    dirs: list[Path] = []
    target = env.get("CARGO_BUILD_TARGET")
    if target:
        dirs.append(ROOT / "target" / target / "debug")
    dirs.append(ROOT / "target" / "debug")
    return dirs


def _find_target_binary(name: str, env: dict[str, str]) -> Path | None:
    binary = _binary_name(name)
    for directory in _target_debug_dirs(env):
        candidate = directory / binary
        if candidate.is_file():
            return candidate
    return None


def _installed_script(name: str) -> Path | None:
    candidate = Path(sys.executable).parent / _binary_name(name)
    if candidate.is_file():
        return candidate
    return None


def _prepare_pytest_binaries(
    env: dict[str, str],
    *,
    prefer_installed_clud: bool,
) -> dict[str, str] | None:
    installed_clud = _installed_script("clud") if prefer_installed_clud else None
    installed_block_guard = (
        _installed_script("clud-block-bad-cmd") if prefer_installed_clud else None
    )
    packages = (
        ["mock-agent"]
        if installed_clud is not None and installed_block_guard is not None
        else ["clud", "mock-agent"]
    )
    cmd = _cargo(["build", *[arg for package in packages for arg in ("-p", package)]], env=env)
    if run(cmd, env=env) != 0:
        return None

    clud_binary = installed_clud or _find_target_binary("clud", env)
    block_guard_binary = installed_block_guard or _find_target_binary("clud-block-bad-cmd", env)
    mock_agent_binary = _find_target_binary("mock-agent", env)
    if clud_binary is None or block_guard_binary is None or mock_agent_binary is None:
        missing = [
            name
            for name, binary in (
                ("clud", clud_binary),
                ("clud-block-bad-cmd", block_guard_binary),
                ("mock-agent", mock_agent_binary),
            )
            if binary is None
        ]
        print(f"missing built pytest binaries: {', '.join(missing)}", file=sys.stderr)
        return None

    # Issue #726: say which binary is under test, always. The failure this
    # prevents is silent — a run against a stale installed wheel looks exactly
    # like a run against your working tree.
    origin = "installed wheel" if installed_clud is not None else "freshly built"
    print(f"[ci.test] clud under test: {clud_binary}  ({origin})", file=sys.stderr)

    pytest_env = env.copy()
    pytest_env["CLUD_TEST_BINARY"] = str(clud_binary)
    pytest_env["CLUD_TEST_BLOCK_BAD_CMD_BINARY"] = str(block_guard_binary)
    pytest_env["CLUD_TEST_MOCK_AGENT_BINARY"] = str(mock_agent_binary)
    return pytest_env


def main(argv: list[str] | None = None) -> int:
    from ci.env import activate, clean_env

    activate()
    env = clean_env()

    argv = list(sys.argv[1:] if argv is None else argv)
    run_unit, run_integration, pytest_args = _select_suites(argv)

    # Rust and Python tests need workspace binaries on disk, but
    # `cargo test --no-run` only compiles test binaries, not workspace bins.
    # Build them once up front and pass their paths into pytest so module
    # collection does not trigger extra cargo builds.
    pytest_env = _prepare_pytest_binaries(
        env,
        # #726: opt-in only. This was `run_integration and not run_unit`, which
        # silently ran the installed wheel for a bare `--integration`.
        prefer_installed_clud=_use_installed_clud(argv),
    )
    if pytest_env is None:
        return 1
    if run_unit:
        if run(_cargo(["test", "--workspace", "--no-run"], env=env), env=env) != 0:
            return 1
        cargo_test = _cargo(["test", "--workspace"], env=env)
        if sys.platform == "win32":
            cargo_test += ["--", "--test-threads=1"]
        if run(cargo_test, env=env) != 0:
            return 1

        # Python unit tests (skip integration by default)
        pytest_cmd = [sys.executable, "-m", "pytest", "-m", "not integration", *pytest_args]
        if not _pytest_ok(run(pytest_cmd, env=pytest_env)):
            return 1

    # Integration tests (only when requested). `-v` prints each test name
    # before it runs so a hang in CI is pinned to the exact test rather than
    # appearing as silent dead air.
    if run_integration:
        int_env = pytest_env.copy()
        int_env["CLUD_INTEGRATION_TESTS"] = "1"
        # Disable the Windows exe-unlock dance for every clud subprocess
        # spawned by tests. See #37: the rename+copy+GC pattern appears to
        # keep stdout/stderr pipe handles alive on Windows CI, which wedges
        # subprocess.run in a pipe-EOF wait. Tests don't need hot-reload
        # protection, so this is strictly safer for the test harness.
        int_env["CLUD_NO_UNLOCK"] = "1"
        # NOT setting CLUD_USE_RUNTIME_CACHE here, deliberately. #333 wants a
        # soak lane for the runtime-cache re-exec hop, but measuring it first
        # showed the hop breaks **31 integration tests** on Windows: the
        # non-Unix branch of `reexec_from_cached_binary` spawns-and-waits
        # instead of `execv`, so it does not preserve the PID, and clud's
        # identity model keys off PIDs throughout. See
        # `runtime_cache.rs::reexec_from_cached_binary` and #333.
        int_cmd = [
            sys.executable,
            "-m",
            "pytest",
            "-m",
            "integration",
            "-v",
            *pytest_args,
        ]
        rc = subprocess.run(int_cmd, cwd=ROOT, env=int_env).returncode
        if not _pytest_ok(rc):
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
