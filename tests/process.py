"""Test-only adapter over running-process.

The test suite must exercise child processes without importing Python's
subprocess module. This adapter exposes the small compatibility surface the
suite needs while delegating every launch and stream drain to running-process.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from running_process import (
    CREATE_NEW_PROCESS_GROUP,
    DEVNULL,
    PIPE,
    STDOUT,
    CompletedProcess,
    PseudoTerminalProcess,
    RunningProcess,
    TimeoutExpired,
    terminate_process_tree,
)

# Win32 constants accepted by running-process's ``creationflags`` argument.
CREATE_NEW_CONSOLE = 0x00000010
CREATE_NO_WINDOW = 0x08000000

__all__ = [
    "CREATE_NEW_CONSOLE",
    "CREATE_NEW_PROCESS_GROUP",
    "CREATE_NO_WINDOW",
    "DEVNULL",
    "PIPE",
    "STDOUT",
    "CompletedProcess",
    "Popen",
    "RunningChild",
    "TimeoutExpired",
    "check_output",
    "run",
    "terminate_process_tree",
]


class _CapturedReader:
    def __init__(self, process: RunningProcess, stream: str) -> None:
        self._process = process
        self._stream = stream

    def read(self) -> str | bytes:
        if isinstance(self._process, PseudoTerminalProcess):
            return self._process.output_text if self._stream == "stdout" else ""
        return self._process.stdout if self._stream == "stdout" else self._process.stderr

    def readline(self) -> str | bytes:
        if isinstance(self._process, PseudoTerminalProcess):
            return ""
        line = (
            self._process.get_next_stdout_line(timeout=0.1)
            if self._stream == "stdout"
            else self._process.get_next_stderr_line(timeout=0.1)
        )
        return line if isinstance(line, (str, bytes)) else ""


class _ChildStdin:
    def __init__(self, process: RunningProcess) -> None:
        self._process = process

    def write(self, data: str | bytes) -> int:
        self._process.write(data)
        return len(data)

    def flush(self) -> None:
        return None

    def close(self) -> None:
        return None


class RunningChild:
    """Compatibility facade for tests that need a live running-process child.

    The facade intentionally exposes no blocking file-object reads. Callers
    needing live output must consume ``next_stdout`` / ``next_stderr`` with a
    finite timeout.
    """

    def __init__(self, args: str | list[str], **kwargs: Any) -> None:
        self.args = args
        self._text = kwargs.get("text", kwargs.get("universal_newlines", False))
        stderr = kwargs.get("stderr")
        stdin = kwargs.get("stdin")
        if isinstance(stdin, int) and stdin not in (PIPE, DEVNULL):
            # running-process intentionally does not adopt raw PTY descriptors.
            # Use its owned PTY instead, which still makes all child stdio a TTY.
            self._process = PseudoTerminalProcess(
                args,
                cwd=kwargs.get("cwd"),
                shell=kwargs.get("shell", False),
                env=kwargs.get("env"),
                capture=True,
            )
        else:
            self._process = RunningProcess(
                args,
                cwd=kwargs.get("cwd"),
                check=False,
                shell=kwargs.get("shell", False),
                env=kwargs.get("env"),
                creationflags=kwargs.get("creationflags"),
                capture=True,
                stdin=stdin,
                stderr=PIPE if stderr is PIPE else None,
                text=self._text,
            )
        self.stdin = _ChildStdin(self._process) if stdin is PIPE else None
        self.stdout = _CapturedReader(self._process, "stdout")
        self.stderr = _CapturedReader(self._process, "stderr")

    @property
    def returncode(self) -> int | None:
        return self._process.returncode

    @property
    def pid(self) -> int | None:
        return self._process.pid

    def poll(self) -> int | None:
        return self._process.poll()

    def wait(self, timeout: float | None = None) -> int:
        try:
            result = self._process.wait(timeout=timeout)
            if isinstance(result, int):
                return result
            raise RuntimeError("idle wait is unsupported by the test process adapter")
        except TimeoutError as exc:
            raise TimeoutExpired(
                self.args,
                timeout if timeout is not None else 0,
                output=self._process.stdout,
                stderr=self._process.stderr,
            ) from exc

    def communicate(
        self, input: str | bytes | None = None, timeout: float | None = None
    ) -> tuple[str | bytes, str | bytes]:
        if input is not None:
            self._process.write(input)
        self._process.wait(timeout=timeout)
        return self._process.stdout, self._process.stderr

    def terminate(self) -> None:
        self._process.terminate()

    def kill(self) -> None:
        self._process.kill()

    def next_stdout(self, timeout: float) -> str | bytes | object:
        return self._process.get_next_stdout_line(timeout=timeout)

    def next_stderr(self, timeout: float) -> str | bytes | object:
        return self._process.get_next_stderr_line(timeout=timeout)


# Preserve familiar test call sites without importing the banned stdlib module.
Popen = RunningChild


def run(*args: Any, **kwargs: Any) -> CompletedProcess[Any]:
    """Run one bounded test command through running-process."""
    if kwargs.get("capture_output"):
        # `running-process` merges stderr by default; mirror subprocess.run.
        kwargs.setdefault("stderr", PIPE)
    return RunningProcess.run(*args, **kwargs)


def check_output(
    args: str | list[str], *, cwd: Path | str | None = None, text: bool = True, **kwargs: Any
) -> str | bytes:
    result = RunningProcess.run(
        args,
        cwd=cwd,
        capture_output=True,
        text=text,
        check=True,
        **kwargs,
    )
    return result.stdout
