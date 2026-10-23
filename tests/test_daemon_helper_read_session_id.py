"""Early-exit diagnostics must not wait for inherited pipe handles to close."""

import pytest

from tests.integration._daemon_helpers import read_session_id


class _PipeKeptOpen:
    def readline(self) -> str:
        return ""

    def read(self) -> str:
        raise AssertionError("unbounded pipe read would hang behind a daemon")


class _ExitedLauncher:
    stdout = _PipeKeptOpen()
    stderr = _PipeKeptOpen()
    returncode = 23

    def poll(self) -> int:
        return self.returncode

    def next_stdout(self, timeout: float) -> str:
        assert 0 < timeout <= 1
        raise TimeoutError

    def next_stderr(self, timeout: float) -> str:
        assert 0 < timeout <= 1
        raise TimeoutError


def test_exited_launcher_diagnostic_never_waits_for_pipe_eof() -> None:
    with pytest.raises(AssertionError, match="exit=23") as failure:
        read_session_id(_ExitedLauncher())
    assert "next_stdout=<no line within 0.2s>" in str(failure.value)
    assert "next_stderr=<no line within 0.2s>" in str(failure.value)


class _QuietPipe:
    def __init__(self, lines: list[str]) -> None:
        self._lines = lines

    def readline(self) -> str:
        return self._lines.pop(0) if self._lines else ""


class _SilentLauncher:
    """A launcher that keeps running and never announces its session."""

    stdout = _QuietPipe([])
    returncode = None

    def __init__(self) -> None:
        self.stderr = _QuietPipe(["[clud] daemon: starting\n"])

    def poll(self) -> None:
        return None

    def next_stdout(self, timeout: float) -> str:
        assert 0 < timeout <= 1
        raise TimeoutError

    def next_stderr(self, timeout: float) -> str:
        assert 0 < timeout <= 1
        raise TimeoutError


def test_a_timeout_reports_what_the_launcher_printed_and_its_stages() -> None:
    # #2000: the timeout used to say only "timed out waiting for daemon
    # session id", with the launcher's output and stage trace thrown away.
    with pytest.raises(AssertionError) as failure:
        read_session_id(
            _SilentLauncher(),
            timeout=0.3,
            describe=lambda: "--- launch stages ---\nlaunch-stage begin daemon-create",
        )
    message = str(failure.value)
    assert "launcher still running" in message
    assert "[clud] daemon: starting" in message
    assert "launch-stage begin daemon-create" in message
