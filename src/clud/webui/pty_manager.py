"""PTY (pseudo-terminal) manager for terminal sessions."""

import contextlib
import logging
import os
import platform
import select
import subprocess
import threading
from collections.abc import Callable
from dataclasses import dataclass, field

logger = logging.getLogger(__name__)


@dataclass
class PTYSession:
    """Represents a PTY session."""

    session_id: str
    pid: int
    fd: int
    cwd: str
    cols: int
    rows: int
    on_output: Callable[[bytes], None]
    on_exit: Callable[[int], None]
    _stop_event: threading.Event = field(default_factory=threading.Event)
    _thread: threading.Thread | None = None


class PTYManager:
    """Manages PTY sessions for terminal access."""

    def __init__(self) -> None:
        """Initialize PTY manager."""
        self.sessions: dict[str, PTYSession] = {}
        self._lock = threading.Lock()

    def create_session(
        self,
        session_id: str,
        cwd: str,
        cols: int,
        rows: int,
        on_output: Callable[[bytes], None],
        on_exit: Callable[[int], None],
    ) -> PTYSession:
        """Create a new PTY session.

        Args:
            session_id: Unique session identifier
            cwd: Working directory for the shell
            cols: Terminal columns
            rows: Terminal rows
            on_output: Callback for terminal output
            on_exit: Callback for process exit

        Returns:
            PTYSession instance

        Raises:
            ValueError: If session already exists
        """
        with self._lock:
            if session_id in self.sessions:
                raise ValueError(f"Session {session_id} already exists")

            # Determine shell command
            shell = self._get_shell()

            # Platform-specific PTY creation
            if platform.system() == "Windows":
                # Use Windows PTY support
                session = self._create_windows_pty(session_id, cwd, cols, rows, shell, on_output, on_exit)
            else:
                # Use Unix PTY support
                session = self._create_unix_pty(session_id, cwd, cols, rows, shell, on_output, on_exit)

            self.sessions[session_id] = session
            logger.info("Created PTY session %s (pid=%d)", session_id, session.pid)

            return session

    def _create_unix_pty(
        self,
        session_id: str,
        cwd: str,
        cols: int,
        rows: int,
        shell: list[str],
        on_output: Callable[[bytes], None],
        on_exit: Callable[[int], None],
    ) -> PTYSession:
        """Create a Unix PTY session.

        Args:
            session_id: Unique session identifier
            cwd: Working directory for the shell
            cols: Terminal columns
            rows: Terminal rows
            shell: Shell command as list
            on_output: Callback for terminal output
            on_exit: Callback for process exit

        Returns:
            PTYSession instance
        """
        import pty

        # Create PTY
        pid, fd = pty.fork()

        if pid == 0:
            # Child process: exec shell
            os.chdir(cwd)
            os.execvp(shell[0], shell)
        else:
            # Parent process: manage PTY
            # Set PTY size
            self._set_pty_size(fd, cols, rows)

            # Create session
            session = PTYSession(
                session_id=session_id,
                pid=pid,
                fd=fd,
                cwd=cwd,
                cols=cols,
                rows=rows,
                on_output=on_output,
                on_exit=on_exit,
            )

            # Start reader thread
            session._thread = threading.Thread(
                target=self._read_loop_unix,
                args=(session,),
                daemon=True,
            )
            session._thread.start()

            return session

    def _create_windows_pty(
        self,
        session_id: str,
        cwd: str,
        cols: int,
        rows: int,
        shell: list[str],
        on_output: Callable[[bytes], None],
        on_exit: Callable[[int], None],
    ) -> PTYSession:
        """Create a Windows PTY session using winpty.

        Args:
            session_id: Unique session identifier
            cwd: Working directory for the shell
            cols: Terminal columns
            rows: Terminal rows
            shell: Shell command as list
            on_output: Callback for terminal output
            on_exit: Callback for process exit

        Returns:
            PTYSession instance
        """
        try:
            import winpty
        except ImportError as e:
            raise RuntimeError("winpty is required for Windows PTY support. Install pywinpty.") from e

        # Create winpty process
        process = winpty.PTY(cols, rows)

        # Build shell command with proper escaping for Windows
        # subprocess.list2cmdline() handles all special characters and quoting
        shell_cmd = subprocess.list2cmdline(shell)

        logger.debug("Spawning Windows PTY with command: %s in directory: %s", shell_cmd, cwd)
        process.spawn(shell_cmd, cwd=cwd)

        # Create session (using process handle as fd for consistency)
        session = PTYSession(
            session_id=session_id,
            pid=process.pid,  # type: ignore[attr-defined]
            fd=0,  # Not used on Windows
            cwd=cwd,
            cols=cols,
            rows=rows,
            on_output=on_output,
            on_exit=on_exit,
        )

        # Store winpty process in session for later use
        session._winpty_process = process  # type: ignore[attr-defined]

        # Start reader thread
        session._thread = threading.Thread(
            target=self._read_loop_windows,
            args=(session,),
            daemon=True,
        )
        session._thread.start()

        return session

    def write_input(self, session_id: str, data: bytes) -> None:
        """Write input to a PTY session.

        Args:
            session_id: Session identifier
            data: Input data to write

        Raises:
            ValueError: If session not found
        """
        with self._lock:
            session = self.sessions.get(session_id)
            if not session:
                raise ValueError(f"Session {session_id} not found")

            try:
                if platform.system() == "Windows":
                    # Windows: use winpty (expects str, not bytes)
                    winpty_process = getattr(session, "_winpty_process", None)
                    if winpty_process:
                        # Convert bytes to str for winpty
                        if isinstance(data, bytes):
                            data_str = data.decode("utf-8", errors="replace")
                            winpty_process.write(data_str)
                        else:
                            winpty_process.write(data)
                else:
                    # Unix: write to FD
                    os.write(session.fd, data)
            except OSError as e:
                logger.error("Error writing to PTY: %s", e)

    def resize(self, session_id: str, cols: int, rows: int) -> None:
        """Resize a PTY session.

        Args:
            session_id: Session identifier
            cols: New column count
            rows: New row count

        Raises:
            ValueError: If session not found
        """
        with self._lock:
            session = self.sessions.get(session_id)
            if not session:
                raise ValueError(f"Session {session_id} not found")

            if platform.system() == "Windows":
                # Windows: use winpty resize
                winpty_process = getattr(session, "_winpty_process", None)
                if winpty_process:
                    winpty_process.set_size(cols, rows)
            else:
                # Unix: use ioctl
                self._set_pty_size(session.fd, cols, rows)

            session.cols = cols
            session.rows = rows
            logger.debug("Resized PTY session %s to %dx%d", session_id, cols, rows)

    def close_session(self, session_id: str) -> None:
        """Close a PTY session.

        Args:
            session_id: Session identifier
        """
        with self._lock:
            session = self.sessions.get(session_id)
            if not session:
                return

            # Signal stop
            session._stop_event.set()

            if platform.system() == "Windows":
                # Windows: cancel I/O and let process terminate
                winpty_process = getattr(session, "_winpty_process", None)
                if winpty_process:
                    with contextlib.suppress(Exception):
                        # Cancel any pending I/O operations
                        winpty_process.cancel_io()
            else:
                # Unix: close FD and kill process
                with contextlib.suppress(OSError):
                    os.close(session.fd)

                with contextlib.suppress(ProcessLookupError):
                    os.kill(session.pid, 15)  # SIGTERM

            # Remove from sessions
            del self.sessions[session_id]

            logger.info("Closed PTY session %s", session_id)

    def _read_loop_unix(self, session: PTYSession) -> None:
        """Read loop for Unix PTY output.

        Args:
            session: PTY session to read from
        """
        try:
            while not session._stop_event.is_set():
                # Use select to check for data with timeout
                readable, _, _ = select.select([session.fd], [], [], 0.1)

                if readable:
                    try:
                        data = os.read(session.fd, 8192)
                        if not data:
                            # EOF
                            break
                        session.on_output(data)
                    except OSError:
                        # FD closed or error
                        break
        except Exception as e:
            logger.exception("Error in PTY read loop: %s", e)
        finally:
            # Wait for process to exit and get status
            try:
                _, status = os.waitpid(session.pid, 0)
                exit_code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else 1
            except (ChildProcessError, AttributeError):
                exit_code = 1

            # Call exit callback
            session.on_exit(exit_code)

    def _read_loop_windows(self, session: PTYSession) -> None:
        """Read loop for Windows PTY output.

        Args:
            session: PTY session to read from
        """
        try:
            winpty_process = getattr(session, "_winpty_process", None)
            if not winpty_process:
                return

            while not session._stop_event.is_set():
                try:
                    # Read with non-blocking call
                    data = winpty_process.read(blocking=False)
                    if data:
                        # winpty returns str, encode to bytes for consistency
                        if isinstance(data, str):
                            session.on_output(data.encode("utf-8"))
                        else:
                            session.on_output(data)
                    else:
                        # No data, small sleep to prevent busy wait
                        import time

                        time.sleep(0.1)
                except Exception as e:
                    logger.error("Error reading from winpty: %s", e)
                    break
        except Exception as e:
            logger.exception("Error in Windows PTY read loop: %s", e)
        finally:
            # Get exit code
            winpty_process = getattr(session, "_winpty_process", None)
            exit_code = 0
            if winpty_process:
                try:
                    exit_code = winpty_process.get_exitstatus()  # type: ignore[attr-defined]
                except Exception:
                    exit_code = 1

            # Call exit callback
            session.on_exit(exit_code)

    @staticmethod
    def _get_shell() -> list[str]:
        """Get shell command for current platform.

        Returns:
            Shell command as list
        """
        if platform.system() == "Windows":
            # On Windows, prefer git-bash if available
            git_bash = r"C:\Program Files\Git\bin\bash.exe"
            if os.path.exists(git_bash):
                return [git_bash, "-l"]
            # Fall back to cmd.exe
            return ["cmd.exe"]
        else:
            # Unix: use user's shell or default to bash
            return [os.environ.get("SHELL", "/bin/bash")]

    @staticmethod
    def _set_pty_size(fd: int, cols: int, rows: int) -> None:
        """Set PTY size on Unix.

        Args:
            fd: PTY file descriptor
            cols: Terminal columns
            rows: Terminal rows
        """
        import fcntl
        import struct
        import termios

        size = struct.pack("HHHH", rows, cols, 0, 0)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, size)
