"""Cross-platform PTY-based agent completion detection."""

import contextlib
import logging
import queue
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from typing import Any

from ..output_filter import OutputFilter

logger = logging.getLogger(__name__)

# Type alias for output callback function
OutputCallback = Callable[[str], None] | None


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


def detect_agent_completion(command: list[str], idle_timeout: float = 3.0, output_callback: OutputCallback = None) -> bool:
    """Detect when a command has completed based on terminal idle state.

    Args:
        command: Command and arguments to execute
        idle_timeout: Number of seconds of terminal idle before considering agent complete
        output_callback: Optional callback to receive output data

    Returns:
        True if command completed due to idle timeout, False if it exited normally
    """
    # Use TerminalState context manager to ensure terminal is restored on exit
    with TerminalState():
        if sys.platform.startswith("win"):
            return _detect_completion_windows(command, idle_timeout, output_callback)
        else:
            return _detect_completion_unix(command, idle_timeout, output_callback)


def _detect_completion_windows(command: list[str], idle_timeout: float, output_callback: OutputCallback) -> bool:
    """Windows PTY detection using pywinpty."""
    try:
        from winpty import PtyProcess  # type: ignore[import-untyped]
    except ImportError as e:
        logger.error(f"pywinpty not available: {e}")
        return _fallback_subprocess_detection(command, idle_timeout, output_callback)

    try:
        cmd_str = " ".join(command)
        process = PtyProcess.spawn(cmd_str)  # type: ignore[misc]
        return _monitor_pty_process(process, idle_timeout, output_callback, "Windows")
    except Exception as e:
        logger.error(f"Windows PTY failed: {e}")
        return _fallback_subprocess_detection(command, idle_timeout, output_callback)


def _detect_completion_unix(command: list[str], idle_timeout: float, output_callback: OutputCallback) -> bool:
    """Unix PTY detection using stdlib pty."""
    try:
        import os
        import pty
    except ImportError as e:
        logger.error(f"Unix PTY modules unavailable: {e}")
        return _fallback_subprocess_detection(command, idle_timeout, output_callback)

    try:
        master, slave = pty.openpty()
        process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, preexec_fn=os.setsid)
        os.close(slave)

        try:
            return _monitor_unix_pty(master, process, idle_timeout, output_callback)
        finally:
            os.close(master)
    except Exception as e:
        logger.error(f"Unix PTY failed: {e}")
        return _fallback_subprocess_detection(command, idle_timeout, output_callback)


def _monitor_pty_process(process: Any, idle_timeout: float, output_callback: OutputCallback, platform: str) -> bool:
    """Monitor Windows PTY process for completion."""
    last_activity = time.time()
    output_filter = OutputFilter()

    try:
        while process.isalive():
            try:
                data = process.read()
                if data:
                    # Always send output to callback (user sees everything)
                    if output_callback:
                        output_callback(data)

                    # Only reset idle timer on meaningful activity
                    if output_filter.is_meaningful(data):
                        last_activity = time.time()
                        logger.debug(f"Meaningful activity detected: {repr(data[:50])}")
                    else:
                        logger.debug(f"TUI noise filtered: {repr(data[:50])}")
                else:
                    time.sleep(0.1)  # No data, avoid busy loop
            except Exception:
                time.sleep(0.1)  # Read error, continue checking

            if time.time() - last_activity > idle_timeout:
                logger.info(f"{platform} agent idle for {idle_timeout}s")
                return True

        return False  # Process exited normally
    except KeyboardInterrupt:
        logger.info(f"{platform} agent monitoring interrupted by user")
        # Try to terminate the process gracefully
        with contextlib.suppress(Exception):
            if hasattr(process, "terminate"):
                process.terminate()
        raise  # Re-raise to allow CLI to handle it


def _monitor_unix_pty(master: int, process: subprocess.Popen[bytes], idle_timeout: float, output_callback: OutputCallback) -> bool:
    """Monitor Unix PTY for completion."""
    import os
    import select

    last_activity = time.time()
    output_filter = OutputFilter()

    try:
        while process.poll() is None:
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    data = os.read(master, 1024)
                    if data:
                        data_str = data.decode("utf-8", errors="replace")

                        # Always send output to callback (user sees everything)
                        if output_callback:
                            output_callback(data_str)

                        # Only reset idle timer on meaningful activity
                        if output_filter.is_meaningful(data_str):
                            last_activity = time.time()
                            logger.debug(f"Meaningful activity detected: {repr(data_str[:50])}")
                        else:
                            logger.debug(f"TUI noise filtered: {repr(data_str[:50])}")
                except OSError:
                    break  # PTY closed

            if time.time() - last_activity > idle_timeout:
                logger.info(f"Unix agent idle for {idle_timeout}s")
                return True

        return False  # Process exited normally
    except KeyboardInterrupt:
        logger.info("Unix agent monitoring interrupted by user")
        # Try to terminate the process gracefully
        with contextlib.suppress(Exception):
            process.terminate()
        raise  # Re-raise to allow CLI to handle it


def _fallback_subprocess_detection(command: list[str], idle_timeout: float, output_callback: OutputCallback) -> bool:
    """Fallback subprocess detection when PTY unavailable."""
    logger.warning("Using subprocess fallback - less reliable than PTY")

    try:
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, universal_newlines=True, bufsize=1)

        output_queue: queue.Queue[str] = queue.Queue()

        # Start output reader thread
        def read_output() -> None:
            if process.stdout:
                try:
                    for line in iter(process.stdout.readline, ""):
                        output_queue.put(line)
                except Exception as e:
                    logger.error(f"Error reading output: {e}")
                finally:
                    process.stdout.close()

        reader_thread = threading.Thread(target=read_output)
        reader_thread.daemon = True
        reader_thread.start()

        # Monitor for idle timeout
        last_activity = time.time()
        output_filter = OutputFilter()

        try:
            while process.poll() is None:
                try:
                    line = output_queue.get(timeout=0.1)

                    # Always send output to callback (user sees everything)
                    if output_callback:
                        output_callback(line)

                    # Only reset idle timer on meaningful activity
                    if output_filter.is_meaningful(line):
                        last_activity = time.time()
                        logger.debug(f"Meaningful activity detected: {repr(line[:50])}")
                    else:
                        logger.debug(f"TUI noise filtered: {repr(line[:50])}")
                except queue.Empty:
                    pass

                if time.time() - last_activity > idle_timeout:
                    logger.info(f"Subprocess agent idle for {idle_timeout}s")
                    return True

            # Process any remaining output
            while not output_queue.empty():
                try:
                    line = output_queue.get_nowait()
                    if output_callback:
                        output_callback(line)
                except queue.Empty:
                    break

            return False  # Process exited normally
        except KeyboardInterrupt:
            logger.info("Subprocess agent monitoring interrupted by user")
            # Try to terminate the process gracefully
            with contextlib.suppress(Exception):
                process.terminate()
            raise  # Re-raise to allow CLI to handle it
    except Exception as e:
        logger.error(f"Subprocess detection failed: {e}")
        return False


# Legacy class-based interface for compatibility
class AgentCompletionDetector:
    """Legacy wrapper for the functional interface."""

    def __init__(self, idle_timeout: float = 3.0) -> None:
        self.idle_timeout = idle_timeout

    def detect_completion(self, command: list[str], output_callback: OutputCallback = None) -> bool:
        return detect_agent_completion(command, self.idle_timeout, output_callback)
