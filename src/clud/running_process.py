"""RunningProcess class for streaming subprocess output."""

import subprocess
import sys
import threading
from collections.abc import Callable
from typing import Any


class RunningProcess:
    """A process wrapper that streams output in real-time instead of capturing it.

    This class is designed to replace subprocess.Popen usage where we want to stream
    output directly to stdout/stderr rather than capturing it.
    """

    def __init__(
        self,
        cmd: list[str],
        env: dict[str, str] | None = None,
        cwd: str | None = None,
        stdout_callback: Callable[[str], None] | None = None,
        stderr_callback: Callable[[str], None] | None = None,
        **kwargs: Any,
    ):
        """Initialize the RunningProcess.

        Args:
            cmd: Command and arguments to execute
            env: Environment variables for the process
            cwd: Working directory for the process
            stdout_callback: Optional callback for stdout lines (default: print to stdout)
            stderr_callback: Optional callback for stderr lines (default: print to stderr)
            **kwargs: Additional arguments passed to subprocess.Popen
        """
        self.cmd = cmd
        self.env = env
        self.cwd = cwd
        self.stdout_callback = stdout_callback or self._default_stdout_callback
        self.stderr_callback = stderr_callback or self._default_stderr_callback

        # Build popen kwargs carefully to avoid type issues
        self.popen_kwargs: dict[str, Any] = {
            "stdout": subprocess.PIPE,
            "stderr": subprocess.PIPE,
            "text": True,
            "bufsize": 1,  # Line buffered
            "universal_newlines": True,
        }

        # Add env and cwd if provided
        if env is not None:
            self.popen_kwargs["env"] = env
        if cwd is not None:
            self.popen_kwargs["cwd"] = cwd

        # Add kwargs while avoiding conflicts with our fixed settings
        reserved_keys = {"stdout", "stderr", "text", "bufsize", "universal_newlines"}
        for key, value in kwargs.items():
            if key not in reserved_keys and key not in {"env", "cwd"}:
                self.popen_kwargs[key] = value

        self.process: subprocess.Popen[str] | None = None
        self.stdout_thread: threading.Thread | None = None
        self.stderr_thread: threading.Thread | None = None
        self.returncode: int | None = None

    def _default_stdout_callback(self, line: str) -> None:
        """Default callback for stdout lines."""
        print(line.rstrip(), flush=True)

    def _default_stderr_callback(self, line: str) -> None:
        """Default callback for stderr lines."""
        print(line.rstrip(), file=sys.stderr, flush=True)

    def _stream_output(self, stream: Any, callback: Callable[[str], None]) -> None:
        """Stream output from a subprocess stream."""
        try:
            for line in iter(stream.readline, ""):
                if line:
                    callback(line)
        except Exception:
            # Stream closed or other error - process may have terminated
            pass

    def start(self) -> None:
        """Start the process and begin streaming output."""
        self.process = subprocess.Popen(self.cmd, **self.popen_kwargs)

        # Start threads to stream stdout and stderr
        if self.process.stdout:
            self.stdout_thread = threading.Thread(target=self._stream_output, args=(self.process.stdout, self.stdout_callback), daemon=True)
            self.stdout_thread.start()

        if self.process.stderr:
            self.stderr_thread = threading.Thread(target=self._stream_output, args=(self.process.stderr, self.stderr_callback), daemon=True)
            self.stderr_thread.start()

    def wait(self, timeout: float | None = None) -> int:
        """Wait for the process to complete and return the exit code.

        Args:
            timeout: Optional timeout in seconds

        Returns:
            Process exit code

        Raises:
            subprocess.TimeoutExpired: If timeout is exceeded
        """
        if not self.process:
            raise RuntimeError("Process not started")

        try:
            self.returncode = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.terminate()
            raise

        # Wait for output threads to finish
        if self.stdout_thread:
            self.stdout_thread.join(timeout=1.0)
        if self.stderr_thread:
            self.stderr_thread.join(timeout=1.0)

        return self.returncode

    def poll(self) -> int | None:
        """Check if the process has terminated.

        Returns:
            Process exit code if terminated, None if still running
        """
        if not self.process:
            return None
        return self.process.poll()

    def terminate(self) -> None:
        """Terminate the process."""
        if self.process:
            self.process.terminate()

    def kill(self) -> None:
        """Kill the process."""
        if self.process:
            self.process.kill()

    def run(self, timeout: float | None = None) -> int:
        """Start the process and wait for completion.

        Args:
            timeout: Optional timeout in seconds

        Returns:
            Process exit code
        """
        self.start()
        return self.wait(timeout=timeout)

    @classmethod
    def run_streaming(
        cls,
        cmd: list[str],
        env: dict[str, str] | None = None,
        cwd: str | None = None,
        timeout: float | None = None,
        stdout_callback: Callable[[str], None] | None = None,
        stderr_callback: Callable[[str], None] | None = None,
        **kwargs: Any,
    ) -> int:
        """Convenience method to run a command with streaming output.

        Args:
            cmd: Command and arguments to execute
            env: Environment variables for the process
            cwd: Working directory for the process
            timeout: Optional timeout in seconds
            stdout_callback: Optional callback for stdout lines
            stderr_callback: Optional callback for stderr lines
            **kwargs: Additional arguments passed to subprocess.Popen

        Returns:
            Process exit code
        """
        process = cls(cmd=cmd, env=env, cwd=cwd, stdout_callback=stdout_callback, stderr_callback=stderr_callback, **kwargs)
        return process.run(timeout=timeout)
