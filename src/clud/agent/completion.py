"""Cross-platform PTY-based agent completion detection."""

import _thread
import contextlib
import logging
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from running_process import PseudoTerminalProcess

from ..output_filter import OutputFilter
from ..util import handle_keyboard_interrupt

logger = logging.getLogger(__name__)

# Type alias for output callback function
OutputCallback = Callable[[str], None] | None

CODEX_CAPACITY_MARKER = "Selected model is at capacity. Please try a different model."
CODEX_CAPACITY_CONTINUE_INPUT = "continue\r"
CODEX_CAPACITY_CONTINUE_LINE = "continue\n"
CODEX_CAPACITY_MAX_RETRIES = 3


@dataclass(slots=True)
class CompletionDetectionResult:
    """Result from running an agent with optional idle-based shutdown."""

    idle_detected: bool
    returncode: int


@dataclass(slots=True)
class _CapacityRetryController:
    """Track deferred Codex capacity retries while the PTY settles."""

    idle_timeout: float
    pending_retry: bool = False
    retry_count: int = 0
    max_retries: int = CODEX_CAPACITY_MAX_RETRIES

    def observe_output(self, data: str) -> None:
        """Flag a retry when Codex emits the temporary capacity marker."""
        if CODEX_CAPACITY_MARKER in data:
            self.pending_retry = True
            logger.info("Detected Codex capacity marker; waiting for PTY to go idle before retrying")

    def maybe_retry(self, last_activity: float, is_alive: bool, send_continue: Callable[[], None], now: float | None = None) -> float | None:
        """Send `continue` once the PTY has been idle long enough."""
        current_time = time.time() if now is None else now
        if not self.pending_retry or not is_alive:
            return None
        if self.retry_count >= self.max_retries:
            logger.warning("Codex capacity retry limit reached (%d)", self.max_retries)
            self.pending_retry = False
            return None
        if current_time - last_activity <= self.idle_timeout:
            return None

        send_continue()
        self.pending_retry = False
        self.retry_count += 1
        logger.info("Sent Codex capacity recovery input (%d/%d)", self.retry_count, self.max_retries)
        return current_time


# Terminal state management
class TerminalState:
    """Context manager for saving and restoring terminal state."""

    def __init__(self) -> None:
        self.saved_state: Any = None
        self.fd: int | None = None

    def __enter__(self) -> "TerminalState":
        """Save the current terminal state."""
        try:
            if sys.platform.startswith("win"):
                self._save_windows_state()
            else:
                self._save_unix_state()
        except Exception as e:
            logger.warning(f"Could not save terminal state: {e}")
        return self

    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> None:
        """Restore the saved terminal state."""
        try:
            if sys.platform.startswith("win"):
                self._restore_windows_state()
            else:
                self._restore_unix_state()
        except Exception as e:
            logger.warning(f"Could not restore terminal state: {e}")

    def _save_unix_state(self) -> None:
        """Save Unix terminal state using termios."""
        try:
            import termios

            fd = sys.stdin.fileno()
            self.fd = fd
            self.saved_state = termios.tcgetattr(fd)
        except Exception:
            # Not a TTY or termios unavailable
            self.fd = None
            self.saved_state = None

    def _restore_unix_state(self) -> None:
        """Restore Unix terminal state using termios."""
        if self.fd is not None and self.saved_state is not None:
            try:
                import termios

                termios.tcsetattr(self.fd, termios.TCSANOW, self.saved_state)
            except Exception as e:
                logger.warning(f"Could not restore Unix terminal state: {e}")

    def _save_windows_state(self) -> None:
        """Save Windows console state."""
        try:
            import ctypes
            from ctypes import wintypes

            # Get handle to stdin
            STD_INPUT_HANDLE = -10
            kernel32 = ctypes.windll.kernel32
            handle = kernel32.GetStdHandle(STD_INPUT_HANDLE)

            # Get current console mode
            mode = wintypes.DWORD()
            if kernel32.GetConsoleMode(handle, ctypes.byref(mode)):
                self.saved_state = (handle, mode.value)
            else:
                self.saved_state = None
        except Exception:
            # Not a console or Windows API unavailable
            self.saved_state = None

    def _restore_windows_state(self) -> None:
        """Restore Windows console state."""
        if self.saved_state is not None:
            try:
                import ctypes

                handle, mode = self.saved_state
                kernel32 = ctypes.windll.kernel32
                kernel32.SetConsoleMode(handle, mode)
            except Exception as e:
                logger.warning(f"Could not restore Windows console state: {e}")


def detect_agent_completion(
    command: list[str],
    idle_timeout: float = 3.0,
    output_callback: OutputCallback = None,
) -> CompletionDetectionResult:
    """Detect when a command has completed based on terminal idle state.

    Uses PseudoTerminalProcess from running-process for cross-platform PTY
    support and centralized process tracking.

    Args:
        command: Command and arguments to execute
        idle_timeout: Number of seconds of terminal idle before considering agent complete
        output_callback: Optional callback to receive output data

    Returns:
        Structured information about whether idle shutdown happened and the exit code.
    """
    # Use TerminalState context manager to ensure terminal is restored on exit
    with TerminalState():
        return _detect_completion_pty(command, idle_timeout, output_callback)


def _detect_completion_pty(
    command: list[str],
    idle_timeout: float,
    output_callback: OutputCallback,
) -> CompletionDetectionResult:
    """PTY-based completion detection using PseudoTerminalProcess."""
    try:
        pty_proc = PseudoTerminalProcess(
            command,
            capture=True,
            rows=24,
            cols=80,
            auto_run=True,
        )
    except Exception as e:
        logger.error(f"PTY process creation failed: {e}")
        return CompletionDetectionResult(idle_detected=False, returncode=1)

    last_activity = time.time()
    saw_meaningful_activity = False
    output_filter = OutputFilter()
    capacity_retry = _CapacityRetryController(idle_timeout=idle_timeout)

    try:
        while pty_proc.poll() is None:
            try:
                chunk = pty_proc.read_non_blocking()
                if chunk is not None:
                    data = chunk.decode("utf-8", errors="replace") if isinstance(chunk, bytes) else chunk
                    capacity_retry.observe_output(data)

                    if output_callback:
                        output_callback(data)

                    if output_filter.is_meaningful(data):
                        last_activity = time.time()
                        saw_meaningful_activity = True
                        logger.debug(f"Meaningful activity detected: {repr(data[:50])}")
                    else:
                        logger.debug(f"TUI noise filtered: {repr(data[:50])}")
                else:
                    time.sleep(0.1)
            except EOFError:
                break
            except Exception:
                time.sleep(0.1)

            retry_time = capacity_retry.maybe_retry(
                last_activity,
                pty_proc.poll() is None,
                lambda: pty_proc.write(CODEX_CAPACITY_CONTINUE_INPUT),
            )
            if retry_time is not None:
                last_activity = retry_time
                continue

            if saw_meaningful_activity and time.time() - last_activity > idle_timeout:
                logger.info(f"Agent idle for {idle_timeout}s")
                with contextlib.suppress(Exception):
                    pty_proc.terminate()
                with contextlib.suppress(Exception):
                    pty_proc.close()
                return CompletionDetectionResult(idle_detected=True, returncode=0)

        returncode = pty_proc.poll() or 0
        return CompletionDetectionResult(idle_detected=False, returncode=returncode)

    except KeyboardInterrupt as e:

        def _cleanup() -> None:
            with contextlib.suppress(Exception):
                pty_proc.terminate()
            with contextlib.suppress(Exception):
                pty_proc.close()
            _thread.interrupt_main()

        handle_keyboard_interrupt(
            e,
            cleanup=_cleanup,
            logger=logger,
            log_message="Agent monitoring interrupted by user",
        )
        return CompletionDetectionResult(idle_detected=False, returncode=130)


# Legacy class-based interface for compatibility
class AgentCompletionDetector:
    """Legacy wrapper for the functional interface."""

    def __init__(self, idle_timeout: float = 3.0) -> None:
        self.idle_timeout = idle_timeout

    def detect_completion(self, command: list[str], output_callback: OutputCallback = None) -> CompletionDetectionResult:
        return detect_agent_completion(command, self.idle_timeout, output_callback)
