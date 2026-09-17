"""Unit tests for what `ci/run_bundle.py` leaves behind when a CI lane fails.

Issue #994: the Windows lane exited 1 with no `FAILED` line and no pytest
summary -- "the process died rather than a test failing" -- and the log said
only `Process completed with exit code 1`. A reader cannot tell a crash from a
hang from an ordinary failure, so every occurrence costs a fresh diagnosis.

An exit code carries more than that if it is read. Two records back it up, both
under the `logs/*` that `_run-tests.yml` uploads: the teed stdout log, which
survives a cancelled job (#1168), and pytest's own junit XML, which names the
failing test when the tee is truncated (#1178).
"""

from __future__ import annotations

import sys
from pathlib import Path

from ci import run_bundle
from ci.run_bundle import (
    _pytest_ok,
    describe_pytest_exit,
    pytest_junit_path,
    pytest_log_path,
    run_pytest,
    run_streamed,
)


def test_pytest_own_codes_are_named() -> None:
    """0-5 are pytest's; say which is which rather than echoing the number."""
    assert "all tests passed" in describe_pytest_exit(0)
    assert "printed a summary" in describe_pytest_exit(1)
    assert "interrupted" in describe_pytest_exit(2)
    assert "internal error" in describe_pytest_exit(3)
    assert "usage error" in describe_pytest_exit(4)
    assert "no tests collected" in describe_pytest_exit(5)


def test_a_posix_signal_is_reported_as_a_crash() -> None:
    """A negative return is a signal, and the missing summary is then expected.

    This is the distinction #994 asks for: a segfault and a failed assertion
    both surface as a non-zero exit, and only one of them means the absent
    summary is a clue worth chasing."""
    described = describe_pytest_exit(-11)
    assert "SIGSEGV" in described
    assert "crashed" in described

    # Named from a fixed table, not the host's `signal.Signals`: Windows has
    # no SIGKILL, so reading a Linux lane's log there would otherwise render
    # the most common CI kill as a bare "signal 9". Caught by the Windows
    # lane doing exactly that.
    assert "SIGKILL" in describe_pytest_exit(-9)
    assert "SIGTERM" in describe_pytest_exit(-15)


def test_a_windows_ntstatus_is_reported_as_a_crash() -> None:
    """Windows reports a crash as a large unsigned code, not a signal.

    A POSIX-shaped `returncode < 0` check sees nothing wrong with
    0xC0000005, which is exactly the platform the issue was filed about."""
    described = describe_pytest_exit(0xC0000005)
    assert "EXCEPTION_ACCESS_VIOLATION" in described
    assert "not a test result" in described

    assert "STATUS_HEAP_CORRUPTION" in describe_pytest_exit(0xC0000374)


def test_an_undocumented_code_says_it_did_not_come_from_pytest() -> None:
    """The most useful thing to say about exit 7 is that pytest cannot produce
    it, so the cause is upstream of pytest's own decision-making."""
    described = describe_pytest_exit(7)
    assert "not one of pytest's documented codes" in described


def test_success_codes_are_unchanged() -> None:
    """The diagnosis is reporting only; it must not redefine success.

    `5` (no tests collected) stays OK because a lane that selects an empty
    marker set is not a failure -- that predates this change and is relied on."""
    assert _pytest_ok(0)
    assert _pytest_ok(5)
    assert not _pytest_ok(1)
    assert not _pytest_ok(-11)


def test_run_streamed_tees_output_to_the_log_as_it_arrives(tmp_path: Path, capsys) -> None:
    """#1168: the cancelled job's step log was empty, so the tee is the record.

    Both streams land in the file (each in its own order -- running-process
    reads them on separate threads, so cross-stream order is not pinned), the
    file is flushed per line so a process killed later still leaves the
    earlier lines, the echo still reaches the step log, and the exit code is
    the child's."""
    log = tmp_path / "nested" / "pytest-unit.log"
    script = (
        "import sys; print('collected 3 items'); "
        "print('E   AssertionError', file=sys.stderr); print('1 failed'); sys.exit(1)"
    )
    rc = run_streamed([sys.executable, "-c", script], {}, log)
    assert rc == 1
    body = log.read_text(encoding="utf-8")
    lines = body.splitlines()
    assert "collected 3 items" in lines
    assert "E   AssertionError" in lines
    assert "1 failed" in lines
    assert lines.index("collected 3 items") < lines.index("1 failed")
    assert len(lines) == 3, body
    assert "collected 3 items" in capsys.readouterr().out


def test_run_streamed_forces_an_unbuffered_child(tmp_path: Path) -> None:
    """A wedged pytest with a block-buffered stdout tees nothing useful;
    the child must be unbuffered so each `-v` line reaches the file."""
    log = tmp_path / "pytest.log"
    rc = run_streamed(
        [sys.executable, "-c", "import os; print(os.environ.get('PYTHONUNBUFFERED'))"], {}, log
    )
    assert rc == 0
    assert log.read_text(encoding="utf-8") == "1\n"


def test_pytest_log_path_is_under_the_uploaded_logs_dir() -> None:
    """`_run-tests.yml` uploads `logs/*`; the tee has to land there."""
    assert pytest_log_path("integration").parent.name == "logs"
    assert pytest_log_path("unit").name == "pytest-unit.log"


def test_pytest_junit_path_is_under_the_uploaded_logs_dir() -> None:
    """#1178: `_run-tests.yml` already uploads `logs/*`, so a pytest-written
    junit report needs no workflow change to reach the artifact."""
    assert pytest_junit_path("integration").parent.name == "logs"
    assert pytest_junit_path("unit").name == "pytest-unit.xml"


def test_run_pytest_asks_pytest_for_its_own_junit_report(monkeypatch) -> None:
    """#1178: the Windows integration lane's teed stdout log stops at 27% and
    never names the failing test; the root cause of that truncation is
    unknown and stays open. A pytest-written junit XML is a workaround --
    `junit_logging=all` puts the captured stdout/stderr of the failing test
    into the XML, so a truncated tee still leaves a machine-readable record
    naming it.

    The new flags must precede the caller's `extra` args so a caller's
    `pytest_args` can still override them -- pytest takes the last
    `--junitxml` / `-o` it sees on the command line."""
    captured: list[tuple[list[str], dict[str, str], Path]] = []

    def fake(argv: list[str], env: dict[str, str], log_path: Path) -> int:
        captured.append((list(argv), env, log_path))
        return 0

    monkeypatch.setattr(run_bundle, "run_streamed", fake)

    env = {"CLUD_INTEGRATION_TESTS": "1"}
    assert run_pytest("integration", env, ["-v"], suite="integration") == 0
    argv, passed_env, log_path = captured[0]

    junit_flag = f"--junitxml={pytest_junit_path('integration')}"
    assert junit_flag in argv
    assert argv[argv.index("-o") + 1] == "junit_logging=all"

    assert argv.index(junit_flag) < argv.index("-v")
    assert argv.index("-o") < argv.index("-v")

    # The junit report is *additional* to the tee, not a replacement: the XML
    # only exists at pytest's sessionfinish, so a cancelled job (#1168) still
    # depends on the teed log. The marker selection and the child env must be
    # untouched too.
    assert argv[:5] == [sys.executable, "-m", "pytest", "-m", "integration"]
    assert log_path == pytest_log_path("integration")
    assert passed_env == env
