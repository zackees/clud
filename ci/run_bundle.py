"""Execute a prebuilt CI bundle on a native runner. Compiles nothing.

This is the exec-side counterpart to `ci.bundle`. It replaces the cargo-driven
orchestration in `ci/test.py` for CI only -- `bash test` still goes through
`ci/test.py` locally, where a toolchain and a `target/` tree exist.

The whole job of this module is to reconstruct the environment the existing
tests already look for, so that no test has to learn a new convention and no
test falls back to building from source:

  CLUD_TEST_BINARY / _BLOCK_BAD_CMD_BINARY / _MOCK_AGENT_BINARY
      checked first by every Python consumer (tests/test_hello.py:58-60,
      tests/integration/conftest.py:181-200, tests/test_hook_stdin.py:36-48)
  CARGO_TARGET_DIR
      read at *runtime* by crates/clud-bin/tests/common/mod.rs:33, so the
      Rust harnesses in pty_pump.rs / pty_behavior.rs / orphan_reap.rs resolve
      mock-agent without a source change
  CLUD_TEST_BIN_DIR
      runtime override for the CARGO_BIN_EXE_* paths that symbols.rs:35,
      telemetry_endpoint.rs:33 and the ctrlc probes bake in at compile time

Design: docs/architecture/ci.md
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import time
from pathlib import Path

from running_process import PseudoTerminalProcess, RunningProcess

from ci import process

ROOT = Path(__file__).resolve().parent.parent

_PYTEST_NO_TESTS_COLLECTED = 5

# pytest's documented exit codes. Anything outside this set did not come from
# pytest deciding something -- it came from the process dying.
_PYTEST_EXIT_MEANINGS = {
    0: "all tests passed",
    1: "tests failed (pytest ran to completion and printed a summary)",
    2: "interrupted (Ctrl-C, or an internal KeyboardInterrupt)",
    3: "internal error in pytest itself",
    4: "pytest usage error (bad arguments)",
    _PYTEST_NO_TESTS_COLLECTED: "no tests collected",
}

# Windows NTSTATUS codes a test process realistically dies on. Reported as a
# large unsigned exit code rather than a signal, so a POSIX-shaped check for a
# negative return sees nothing wrong.
_NTSTATUS_MEANINGS = {
    0xC0000005: "EXCEPTION_ACCESS_VIOLATION",
    0xC000001D: "EXCEPTION_ILLEGAL_INSTRUCTION",
    0xC00000FD: "EXCEPTION_STACK_OVERFLOW",
    0xC0000374: "STATUS_HEAP_CORRUPTION",
    0xC0000409: "STATUS_STACK_BUFFER_OVERRUN",
    0xC000013A: "STATUS_CONTROL_C_EXIT",
}


def describe_pytest_exit(returncode: int) -> str:
    """Say what a pytest exit code means, especially when it is not pytest's.

    Issue #994: the Windows lane exited 1 with no `FAILED` line and no pytest
    summary -- "the process died rather than a test failing" -- and the log
    said only `Process completed with exit code 1`. A reader cannot tell a
    crash from a hang from an ordinary failure, so every occurrence costs a
    fresh diagnosis.

    An exit code carries more than that if it is read. A negative value is a
    POSIX signal; a large unsigned one is an NTSTATUS; and codes outside
    pytest's documented set did not come from pytest at all.
    """
    if returncode < 0:
        signum = -returncode
        name = _POSIX_SIGNAL_NAMES.get(signum, f"signal {signum}")
        return (
            f"exit {returncode}: killed by {name} -- the process crashed or was "
            f"killed, so any missing summary is expected rather than a clue"
        )

    unsigned = returncode & 0xFFFFFFFF
    if unsigned in _NTSTATUS_MEANINGS:
        return (
            f"exit {returncode} (0x{unsigned:08X}): {_NTSTATUS_MEANINGS[unsigned]} "
            f"-- a Windows crash, not a test result"
        )

    if returncode in _PYTEST_EXIT_MEANINGS:
        return f"exit {returncode}: {_PYTEST_EXIT_MEANINGS[returncode]}"

    return (
        f"exit {returncode}: not one of pytest's documented codes (0-5), so "
        f"this did not come from pytest deciding anything -- suspect the "
        f"process dying, a wrapper, or a timeout killer"
    )


# Named from a fixed table rather than `signal.Signals`, which is the *host's*
# signal set. A negative exit code can only have come from a POSIX runner, but
# the log may well be read on Windows -- where `SIGKILL` does not exist, so the
# host enum would render the most common CI kill as a bare "signal 9". The
# number means the same thing wherever it is interpreted, so the name should
# too.
_POSIX_SIGNAL_NAMES = {
    1: "SIGHUP",
    2: "SIGINT",
    3: "SIGQUIT",
    4: "SIGILL",
    6: "SIGABRT",
    8: "SIGFPE",
    9: "SIGKILL",
    11: "SIGSEGV",
    13: "SIGPIPE",
    14: "SIGALRM",
    15: "SIGTERM",
    24: "SIGXCPU",
    25: "SIGXFSZ",
}


def _pytest_ok(returncode: int) -> bool:
    return returncode in (0, _PYTEST_NO_TESTS_COLLECTED)


def _bin_dir(bundle: Path, manifest: dict) -> Path:
    return bundle / "target" / manifest["profile_dir"]


def _exe(name: str, manifest: dict) -> str:
    return f"{name}.exe" if "windows" in manifest["target"] else name


def bundle_env(bundle: Path, manifest: dict) -> dict[str, str]:
    bin_dir = _bin_dir(bundle, manifest)
    env = os.environ.copy()
    env.pop("VIRTUAL_ENV", None)
    env.setdefault("PYTHONUTF8", "1")
    env["RUST_BACKTRACE"] = "1"
    env["CARGO_TARGET_DIR"] = str((bundle / "target").resolve())
    env["CLUD_TEST_BIN_DIR"] = str(bin_dir.resolve())
    env["CLUD_TEST_BINARY"] = str((bin_dir / _exe("clud", manifest)).resolve())
    env["CLUD_TEST_BLOCK_BAD_CMD_BINARY"] = str(
        (bin_dir / _exe("clud-block-bad-cmd", manifest)).resolve()
    )
    env["CLUD_TEST_MOCK_AGENT_BINARY"] = str((bin_dir / _exe("mock-agent", manifest)).resolve())
    return env


def stage_wheel(bundle: Path) -> None:
    """Put the bundled wheel where `ci.build_wheel.latest_wheel()` looks.

    tests/integration/test_trampoline.py:27-44 hard-`fail`s (not skips) when
    dist/ has no wheel, so this is required coverage, not a nicety.
    """
    src = bundle / "dist"
    if not src.is_dir():
        return
    dest = ROOT / "dist"
    dest.mkdir(parents=True, exist_ok=True)
    for wheel in src.glob("*.whl"):
        shutil.copy2(wheel, dest / wheel.name)


def install_wheel(bundle: Path, env: dict[str, str]) -> int:
    """Install the bundled wheel and smoke-test its console scripts.

    The old `_integration-test.yml` got this for free because it ran
    `ci.build_wheel --dev`, which installs and then calls
    `verify_installed_scripts` (ci/build_wheel.py:115-174). The exec runner does
    not build, so the install + smoke test is done explicitly here to keep that
    coverage.
    """
    from ci.build_wheel import install_wheel as do_install
    from ci.build_wheel import latest_wheel

    wheel = latest_wheel()
    print(f"installing bundled wheel: {wheel.name}", flush=True)
    return do_install(wheel, env=env)


# The Rust harness whose tests drive a real PTY (crates/clud-bin/tests/pty/).
# Cargo names its binary `pty-<hash>[.exe]`.
TERMINAL_HARNESS = "pty"
# Makes `require_pty_or_skip!` fail instead of skip (tests/common/mod.rs).
REQUIRE_PTY_ENV = "CLUD_REQUIRE_PTY"
# Upper bound on one test of the terminal harness. Each test runs in its own
# pseudo-terminal (see `run_terminal_harness`), so a hang costs this much and
# names the test instead of swallowing the rest of the harness.
TERMINAL_TEST_TIMEOUT_SECS = 60.0


def needs_terminal(harness: Path) -> bool:
    """True for the harness that must run with a terminal as its stdout.

    #691: ConPTY stops relaying child output when the *spawning* process's
    stdout is a pipe, which is what `process.run` gives every harness. The PTY
    tests then skipped silently on Windows, so the configuration interactive
    launches ship -- clud under a real terminal -- had no coverage there.
    """
    name, sep, _hash = harness.name.removesuffix(".exe").rpartition("-")
    return bool(sep) and name == TERMINAL_HARNESS


def run_in_terminal(
    argv: list[str], env: dict[str, str], timeout: float = TERMINAL_TEST_TIMEOUT_SECS
) -> int:
    """Run `argv` inside a pseudo-terminal, echoing its output as it arrives.

    Inside the pseudo-terminal the harness's stdin and stdout are a console on
    Windows and a TTY on POSIX, exactly as when a user launches clud, so the
    PTY canary is expected to pass and `CLUD_REQUIRE_PTY=1` turns any failure
    into a red test instead of a skip.
    """
    child_env = dict(env)
    child_env[REQUIRE_PTY_ENV] = "1"
    terminal = PseudoTerminalProcess(
        argv, cwd=ROOT, env=child_env, capture=True, rows=50, cols=200
    )
    deadline = time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            try:
                sys.stdout.write(terminal.read_text(timeout=0.5))
                sys.stdout.flush()
            except TimeoutError:
                # ConPTY keeps its output pipe open after the child exits, so
                # EOF may never come on Windows: an exited child is the end.
                if terminal.poll() is not None:
                    _echo_remaining(terminal)
                    break
            except EOFError:
                break
        else:
            print(
                f"::error::{' '.join(argv)} did not finish within "
                f"{timeout:.0f}s inside the pseudo-terminal",
                file=sys.stderr,
            )
            terminal.kill()
            return 1
        return terminal.wait(timeout=30)
    finally:
        terminal.close()


def _echo_remaining(terminal: PseudoTerminalProcess) -> None:
    """Echo whatever output is already buffered, without waiting for more."""
    while True:
        try:
            chunk = terminal.read_non_blocking()
        except EOFError:
            return
        if not chunk:
            return
        text = chunk.decode(terminal.encoding, "replace") if isinstance(chunk, bytes) else chunk
        sys.stdout.write(text)
        sys.stdout.flush()


def list_tests(harness: Path, env: dict[str, str]) -> list[str]:
    """Names of the tests in a libtest harness (`--list --format terse`)."""
    result = process.run(
        [str(harness), "--list", "--format", "terse"],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
    )
    return [
        line.removesuffix(": test")
        for line in (result.stdout or "").splitlines()
        if line.endswith(": test")
    ]


def dump_traces(name: str, trace_dir: Path) -> None:
    """Print the mock-agent stage traces a failed or hung PTY test left (#1310)."""
    logs = sorted(trace_dir.glob("*.log")) if trace_dir.is_dir() else []
    if not logs:
        print(f"[pty-trace] {name}: no mock-agent trace (child never started?)", flush=True)
        return
    for log in logs:
        print(f"[pty-trace] {name}: {log.name}", flush=True)
        for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
            print(f"[pty-trace]   {line}", flush=True)


def run_terminal_harness(argv: list[str], env: dict[str, str]) -> int:
    """Run each test of the terminal harness in its own pseudo-terminal.

    #1310: one hung PTY test used to hold the whole harness until its
    timeout, and the kill discarded every earlier failure message. One test
    per terminal bounds each hang and keeps each verdict and message.
    """
    names = list_tests(Path(argv[0]), env)
    if not names:
        print(f"::error::{argv[0]} listed no tests", file=sys.stderr)
        return 1
    failed = []
    for name in names:
        trace_dir = LOG_DIR / "pty-trace" / name.replace("::", "__")
        test_env = dict(env)
        test_env["MOCK_AGENT_TRACE_DIR"] = str(trace_dir)
        test_env["CLUD_PTY_PUMP_TRACE"] = "1"
        if run_in_terminal([*argv, "--exact", name, "--nocapture"], test_env) != 0:
            failed.append(name)
            dump_traces(name, trace_dir)
    if failed:
        print(f"::error::failing PTY tests: {', '.join(failed)}", file=sys.stderr)
        return 1
    return 0


def run_harnesses(bundle: Path, manifest: dict, env: dict[str, str]) -> int:
    """Run every `cargo test --no-run` harness binary shipped in the bundle."""
    tests_dir = bundle / "tests"
    harnesses = sorted(tests_dir.glob("*"))
    if not harnesses:
        print("bundle contains no test harnesses", file=sys.stderr)
        return 1

    failures: list[str] = []
    for harness in harnesses:
        if not harness.is_file():
            continue
        argv = [str(harness)]
        # Mirrors ci/test.py:138-139 -- the Rust suite is not parallel-safe on
        # Windows (shared console/PTY state).
        if sys.platform == "win32":
            argv += ["--test-threads=1"]
        print(f"::group::{harness.name}", flush=True)
        if needs_terminal(harness):
            rc = run_terminal_harness(argv, env)
        else:
            rc = process.run(argv, cwd=ROOT, env=env).returncode
        print("::endgroup::", flush=True)
        if rc != 0:
            failures.append(f"{harness.name} (rc={rc})")

    if failures:
        print(f"::error::failing Rust harnesses: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


LOG_DIR = ROOT / "logs"


def pytest_log_path(suite: str) -> Path:
    """Where `run_pytest` tees the suite's output; uploaded by `_run-tests.yml`."""
    return LOG_DIR / f"pytest-{suite}.log"


def pytest_junit_path(suite: str) -> Path:
    """Where pytest writes its own junit XML report.

    Same `logs/` dir `_run-tests.yml` already uploads, so no workflow change.
    """
    return LOG_DIR / f"pytest-{suite}.xml"


def run_streamed(argv: list[str], env: dict[str, str], log_path: Path) -> int:
    """Run `argv`, echoing each output line as it arrives and teeing it to a file.

    #1168: when the Windows integration job wedged past the 20-minute ceiling,
    the cancelled step's log was empty -- GitHub keeps no log for a cancelled
    step -- so nothing named the test that hung. The step's own stdout cannot
    survive that. A file can: `_run-tests.yml` uploads `logs/` with
    `if: always()`, which does run on cancellation, so the partial output
    (including any faulthandler stack dump, which lands on stderr and is
    merged in here) reaches the artifact even though the log did not.

    running-process streams the child's stdout and stderr as one line
    sequence, so the tee sees output in arrival order with no buffering of
    its own; `flush=True` on the echo and `PYTHONUNBUFFERED` on the child
    close the other two buffers between pytest and the file.
    """
    log_path.parent.mkdir(parents=True, exist_ok=True)
    child_env = dict(env)
    child_env["PYTHONUNBUFFERED"] = "1"
    proc = RunningProcess(argv, cwd=ROOT, env=child_env)
    with log_path.open("w", encoding="utf-8", errors="replace") as log:
        for line in proc.line_iter(timeout=None):
            print(line, flush=True)
            log.write(line + "\n")
            log.flush()
    return proc.wait()


def run_pytest(marker: str, env: dict[str, str], extra: list[str], *, suite: str) -> int:
    """Run pytest, teed to a log and also writing its own junit XML report.

    #1178: on the Windows integration lane the teed stdout record stopped at
    27% and never named the failing test, even though the step reported
    pytest ran to completion. pytest writing its own junit XML gives a
    second, structured record of the same run, and `junit_logging=all` puts
    each test's captured stdout/stderr/log into it. That XML is written only
    at pytest's `sessionfinish`, so it complements the tee (which survives a
    cancelled job) rather than replacing it. This is a workaround; the
    truncation's root cause stays unknown and open on #1178.
    """
    argv = [
        sys.executable,
        "-m",
        "pytest",
        "-m",
        marker,
        f"--junitxml={pytest_junit_path(suite)}",
        "-o",
        "junit_logging=all",
        *extra,
    ]
    return run_streamed(argv, env, pytest_log_path(suite))


def report_pytest_exit(returncode: int) -> bool:
    """Print what the exit code means, and say whether it counts as success.

    #994: `Process completed with exit code 1` is the whole story a reader
    gets today, and it reads the same whether pytest failed a test, crashed,
    or was killed. One line of interpretation is the difference between
    "is this mine?" costing a log download and costing nothing.
    """
    ok = _pytest_ok(returncode)
    if not ok:
        print(f"::error::pytest {describe_pytest_exit(returncode)}", file=sys.stderr)
    return ok


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run a prebuilt CI test bundle")
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--suite", choices=("unit", "integration", "harness"), required=True)
    parser.add_argument("pytest_args", nargs="*")
    args = parser.parse_args(argv)

    bundle = args.bundle.resolve()
    manifest = json.loads((bundle / "manifest.json").read_text(encoding="utf-8"))
    env = bundle_env(bundle, manifest)
    stage_wheel(bundle)

    if args.suite == "unit":
        if run_harnesses(bundle, manifest, env) != 0:
            return 1
        rc = run_pytest("not integration", env, args.pytest_args, suite="unit")
        return 0 if report_pytest_exit(rc) else 1

    if args.suite == "harness":
        # #1323: real Claude Code on `mock-agent serve`. The fixture runs the
        # bundle's own clud and mock-agent; `_run-tests.yml` installs the
        # pinned Claude Code for this suite only.
        env = env.copy()
        bin_dir = Path(env["CLUD_TEST_MOCK_AGENT_BINARY"]).parent
        env["CLUD_HARNESS_CLUD"] = str(bin_dir / _exe("clud", manifest))
        env["CLUD_HARNESS_MOCK_AGENT"] = str(bin_dir / _exe("mock-agent", manifest))
        env["CLUD_REAL_CLAUDE_TESTS"] = "1"
        return (
            0
            if report_pytest_exit(
                run_pytest(
                    "real_claude", env, ["-v", "tests/harness", *args.pytest_args], suite="harness"
                )
            )
            else 1
        )

    if install_wheel(bundle, env) != 0:
        return 1
    env = env.copy()
    # Point the integration suite at the *installed console scripts*, not the
    # raw bundle binaries. ci/test.py:74-87,130-131 did this deliberately
    # (`prefer_installed_clud`) so integration exercises the packaged
    # trampoline path rather than a bare target/ binary. mock-agent is not
    # part of the wheel, so it keeps pointing into the bundle.
    for name, var in (
        ("clud", "CLUD_TEST_BINARY"),
        ("clud-block-bad-cmd", "CLUD_TEST_BLOCK_BAD_CMD_BINARY"),
    ):
        installed = Path(sys.executable).parent / _exe(name, manifest)
        if installed.is_file():
            env[var] = str(installed)
    env["CLUD_INTEGRATION_TESTS"] = "1"
    # See ci/test.py:154-159 (#37): the Windows exe-unlock rename+copy+GC dance
    # keeps stdout/stderr pipe handles alive on Windows CI and wedges
    # process.run in a pipe-EOF wait. Tests do not need hot-reload
    # protection.
    env["CLUD_NO_UNLOCK"] = "1"
    # `-v` prints each test name before it runs, so a hang is pinned to an exact
    # test rather than showing up as silent dead air before the job timeout.
    return (
        0
        if report_pytest_exit(
            run_pytest("integration", env, ["-v", *args.pytest_args], suite="integration")
        )
        else 1
    )


if __name__ == "__main__":
    sys.exit(main())
