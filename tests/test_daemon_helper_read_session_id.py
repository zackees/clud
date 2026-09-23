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
