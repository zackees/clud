"""Compatibility contracts for the repository's running-process adapters."""

from __future__ import annotations

import sys
import time

import pytest

from ci import process as ci_process
from tests import process as test_process


@pytest.mark.parametrize("adapter", [ci_process, test_process])
def test_capture_output_keeps_stdout_and_stderr_separate(adapter) -> None:
    result = adapter.run(
        [
            sys.executable,
            "-c",
            "import sys; print('stdout'); print('stderr', file=sys.stderr)",
        ],
        capture_output=True,
        text=True,
        check=True,
    )

    assert result.stdout == "stdout"
    assert result.stderr == "stderr"


@pytest.mark.parametrize("adapter", [ci_process, test_process])
def test_capture_output_defaults_to_bytes_like_subprocess(adapter) -> None:
    result = adapter.run(
        [sys.executable, "-c", "import sys; sys.stdout.buffer.write(b'raw')"],
        capture_output=True,
        check=True,
    )

    assert result.stdout == b"raw"
    assert isinstance(result.stdout, bytes)


def test_run_supports_devnull_streams() -> None:
    result = test_process.run(
        [sys.executable, "-c", "print('discarded')"],
        stdin=test_process.DEVNULL,
        stdout=test_process.DEVNULL,
        stderr=test_process.DEVNULL,
        timeout=5,
    )

    assert result.returncode == 0
    assert result.stdout is None
    assert result.stderr is None


@pytest.mark.skipif(sys.platform != "win32", reason="Windows creation flag")
def test_run_supports_creationflags() -> None:
    result = test_process.run(
        [sys.executable, "-c", "print('ok')"],
        capture_output=True,
        timeout=5,
        creationflags=test_process.CREATE_NO_WINDOW,
    )

    assert result.returncode == 0
    assert result.stdout == b"ok"
    assert isinstance(result.stdout, bytes)


def test_live_reader_returns_control_after_a_bounded_wait() -> None:
    child = test_process.Popen(
        [sys.executable, "-c", "import time; time.sleep(2)"],
        stdout=test_process.PIPE,
        text=True,
    )
    started = time.monotonic()
    try:
        assert child.stdout is not None
        assert child.stdout.readline() == ""
        assert time.monotonic() - started < 1
    finally:
        child.kill()
        child.wait(timeout=5)


def test_child_interrupt_exit_code_is_returned_instead_of_raising() -> None:
    child = test_process.Popen([sys.executable, "-c", "raise SystemExit(130)"])

    assert child.wait(timeout=5) == 130


def test_one_sided_devnull_is_rejected_instead_of_changing_other_stream() -> None:
    with pytest.raises(NotImplementedError, match="DEVNULL only for both output streams"):
        test_process.run(
            [sys.executable, "-c", "print('stdout')"],
            stdout=test_process.DEVNULL,
            timeout=5,
        )
