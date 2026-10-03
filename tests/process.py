"""Test-only adapter over running-process.

The test suite must exercise child processes without importing Python's
subprocess module. This adapter exposes the small compatibility surface the
suite needs while delegating every launch and stream drain to running-process.
"""

from __future__ import annotations

import signal
from collections import defaultdict
from collections.abc import Iterable
from pathlib import Path
from typing import Any

import psutil
from running_process import (
    CREATE_NEW_PROCESS_GROUP,
    DEVNULL,
    PIPE,
    STDOUT,
    CompletedProcess,
    PseudoTerminalProcess,
    RunningProcess,
    TimeoutExpired,
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
            return "" if self._process.text else b""
        try:
            line = (
                self._process.get_next_stdout_line(timeout=0.1)
                if self._stream == "stdout"
                else self._process.get_next_stderr_line(timeout=0.1)
            )
        except TimeoutError:
            return "" if self._process.text else b""
        return line if isinstance(line, (str, bytes)) else (
            "" if self._process.text else b""
        )


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
        self._text = bool(
            kwargs.get("text", False)
            or kwargs.get("universal_newlines", False)
            or kwargs.get("encoding") is not None
            or kwargs.get("errors") is not None
        )
        stdout = kwargs.get("stdout")
        stderr = kwargs.get("stderr")
        stdin = kwargs.get("stdin")
        uses_pty = isinstance(stdin, int) and stdin not in (PIPE, DEVNULL)
        capture = kwargs.get("capture")
        if capture is None:
            capture = stdout in (PIPE, DEVNULL) or stderr in (PIPE, DEVNULL)
        if uses_pty:
            # running-process intentionally does not adopt raw PTY descriptors.
            # Use its owned PTY instead, which still makes all child stdio a TTY.
            self._process = PseudoTerminalProcess(
                args,
                cwd=kwargs.get("cwd"),
                shell=kwargs.get("shell", False),
                env=kwargs.get("env"),
                capture=True,
                text=self._text,
                encoding=kwargs.get("encoding", "utf-8"),
                errors=kwargs.get("errors", "replace"),
            )
        else:
            self._process = RunningProcess(
                args,
                cwd=kwargs.get("cwd"),
                check=False,
                shell=kwargs.get("shell", False),
                env=kwargs.get("env"),
                creationflags=kwargs.get("creationflags"),
                capture=capture,
                stdin=stdin,
                stderr=PIPE if stderr is PIPE else None,
                text=self._text,
                encoding=kwargs.get("encoding"),
                errors=kwargs.get("errors"),
                universal_newlines=kwargs.get("universal_newlines", False),
            )
        self.stdin = _ChildStdin(self._process) if stdin is PIPE else None
        self.stdout = (
            _CapturedReader(self._process, "stdout")
            if stdout is PIPE or uses_pty
            else None
        )
        self.stderr = _CapturedReader(self._process, "stderr") if stderr is PIPE else None

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
            try:
                result = self._process.wait(timeout=timeout)
            except KeyboardInterrupt:
                # running-process treats child Ctrl+C exit statuses as a
                # KeyboardInterrupt. Popen.wait() returns the status instead;
                # distinguish that case from an actual interrupt of pytest.
                result = self._process.returncode
                if result not in RunningProcess.KEYBOARD_INTERRUPT_EXIT_CODES:
                    raise
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
        self.wait(timeout=timeout)
        return self._process.stdout, self._process.stderr

    def terminate(self) -> None:
        self._process.terminate()

    def kill(self) -> None:
        self._process.kill()

    def send_signal(self, sig: int) -> None:
        interrupt_signals = {signal.SIGINT}
        ctrl_break = getattr(signal, "CTRL_BREAK_EVENT", None)
        if ctrl_break is not None:
            interrupt_signals.add(ctrl_break)
        if sig not in interrupt_signals:
            raise NotImplementedError(
                "RunningChild.send_signal only supports SIGINT/CTRL_BREAK_EVENT"
            )
        self._process.send_interrupt()

    def next_stdout(self, timeout: float) -> str | bytes | object:
        return self._process.get_next_stdout_line(timeout=timeout)

    def next_stderr(self, timeout: float) -> str | bytes | object:
        return self._process.get_next_stderr_line(timeout=timeout)


# Preserve familiar test call sites without importing the banned stdlib module.
Popen = RunningChild


def index_children(
    rows: Iterable[tuple[int, int | None, float | None]],
) -> dict[int, list[int]]:
    """Parent -> children over `(pid, ppid, create_time)` rows, real links only.

    Windows never rewrites a parent PID when the parent exits, and it recycles
    PIDs, so a process whose parent died names whatever later process holds
    that PID as its parent. A real child is never older than its parent; a
    child that is, or whose age cannot be read, is not linked (#1738).
    """
    table = {pid: (ppid, created) for pid, ppid, created in rows}
    children: dict[int, list[int]] = defaultdict(list)
    for pid, (ppid, created) in table.items():
        if ppid is None or ppid == pid or ppid not in table or created is None:
            continue
        parent_created = table[ppid][1]
        if parent_created is not None and created >= parent_created:
            children[ppid].append(pid)
    return dict(children)


def descendants(children: dict[int, list[int]], root: int) -> list[int]:
    """Every PID below `root` in `children`, each parent before its children."""
    order: list[int] = []
    seen = {root}
    stack = [root]
    while stack:
        for child in children.get(stack.pop(), []):
            if child not in seen:
                seen.add(child)
                order.append(child)
                stack.append(child)
    return order


def terminate_process_tree(pid: int, timeout_seconds: float = 3.0) -> bool:
    """Kill `pid` and its descendants, leaves first; True once all are gone.

    Not `running_process.terminate_process_tree`: that walks raw parent PIDs.
    When a clud daemon was handed the recycled PID of a dead ancestor of the
    pytest runner, that walk went up into the runner and killed pytest itself,
    which exited 1 with no summary (#1738). This walk goes through
    `index_children`, which only links a child no older than its parent.
    """
    procs = {proc.pid: proc for proc in psutil.process_iter(["ppid", "create_time"])}
    root = procs.get(pid)
    if root is None:
        return True
    children = index_children(
        (child, proc.info["ppid"], proc.info["create_time"]) for child, proc in procs.items()
    )
    targets = [procs[child] for child in reversed(descendants(children, pid))]
    targets.append(root)
    for target in targets:
        try:
            # psutil re-checks the creation time it cached during the walk,
            # so a PID recycled since then is refused instead of killed.
            target.kill()
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    _gone, alive = psutil.wait_procs(targets, timeout=timeout_seconds)
    return not alive


def run(*args: Any, **kwargs: Any) -> CompletedProcess[Any]:
    """Run one bounded test command through running-process."""
    # running-process defaults to decoded text while subprocess.run defaults
    # to bytes. This module is a subprocess compatibility surface, so opt into
    # text only when the caller names one of subprocess's text channels.
    kwargs.setdefault(
        "text",
        bool(
            kwargs.get("encoding") is not None
            or kwargs.get("errors") is not None
            or kwargs.get("universal_newlines", False)
        ),
    )
    if kwargs.get("capture_output"):
        # `running-process` merges stderr by default; mirror subprocess.run.
        kwargs.setdefault("stderr", PIPE)

    # RunningProcess.run intentionally exposes a narrower subprocess.run
    # surface than its live-process constructor.  The test suite needs the
    # constructor-only Windows creation flags, and a few smoke tests discard
    # output with DEVNULL.  Route those cases through the compatibility facade
    # instead of passing unsupported kwargs into RunningProcess.run.
    if (
        kwargs.get("creationflags") is not None
        or kwargs.get("stdout") is DEVNULL
        or kwargs.get("stderr") is DEVNULL
    ):
        options = dict(kwargs)
        input_data = options.pop("input", None)
        timeout = options.pop("timeout", None)
        check = bool(options.pop("check", False))
        capture_output = bool(options.pop("capture_output", False))
        stdout_mode = options.pop("stdout", None)
        stderr_mode = options.pop("stderr", None)
        if (stdout_mode is DEVNULL) != (stderr_mode is DEVNULL):
            raise NotImplementedError(
                "the test process adapter supports DEVNULL only for both output streams"
            )
        if input_data is not None:
            if options.get("stdin") is not None:
                raise ValueError("stdin and input arguments may not both be used.")
            options["stdin"] = PIPE

        # RunningProcess has no direct stdout/stderr DEVNULL mode. Capturing
        # and discarding is equivalent at this test boundary and prevents the
        # child from inheriting either console handle.
        if stderr_mode is DEVNULL:
            options["stderr"] = PIPE
        elif stderr_mode is not None:
            options["stderr"] = stderr_mode
        options["stdout"] = stdout_mode
        options["capture"] = (
            capture_output
            or stdout_mode in (PIPE, DEVNULL)
            or stderr_mode in (PIPE, DEVNULL)
        )
        options.pop("close_fds", None)

        child = RunningChild(args[0], **options)
        stdout_value, stderr_value = child.communicate(input=input_data, timeout=timeout)
        completed = CompletedProcess(
            args=args[0],
            returncode=child.returncode if child.returncode is not None else child.wait(),
            stdout=(
                stdout_value
                if capture_output or stdout_mode is PIPE
                else None
            ),
            stderr=(stderr_value if stderr_mode is PIPE else None),
        )
        if check:
            completed.check_returncode()
        return completed
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
