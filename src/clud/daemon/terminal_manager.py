"""Terminal manager for PTY process management in multi-terminal daemon.

Handles PTY process creation, communication, and lifecycle for both Windows
(using pywinpty) and Unix (using pty/os) platforms.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import platform
import sys
from pathlib import Path
from typing import Any, cast

from ..output_filter import OutputFilter
from .input_buffer import InputSnapshot, TerminalInputTracker

logger = logging.getLogger(__name__)


class Terminal:
    """Manages a single PTY terminal session.

    Handles PTY process creation, stdin/stdout communication, and WebSocket
    bridging for interactive terminal sessions.

    Attributes:
        terminal_id: Unique identifier for this terminal
        cwd: Working directory for the terminal process
        is_running: Whether the PTY process is currently running
    """

    def __init__(self, terminal_id: int, cwd: str | None = None) -> None:
        """Initialize a terminal session.

        Args:
            terminal_id: Unique identifier for this terminal
            cwd: Working directory for the terminal process. Defaults to user's home.
        """
        self.terminal_id = terminal_id
        self.cwd = cwd or str(Path.home())
        self.is_running = False

        # PTY-related state (initialized in start())
        self._pty_process: Any = None  # PtyProcess on Windows, pid on Unix
        self._pty_fd: int | None = None
        self._read_task: asyncio.Task[None] | None = None
        self._websocket: Any = None  # websockets.WebSocketServerProtocol
        self._cols: int = 80
        self._rows: int = 24
        # Suppress terminal capability response sequences from client input.
        self._input_filter = OutputFilter()
        self._input_tracker = TerminalInputTracker()

    def start(self) -> bool:
        """Start the PTY process.

        Creates a new PTY process with the appropriate shell for the platform.
        On Windows, uses pywinpty. On Unix, uses the standard pty module.

        Returns:
            True if the process started successfully, False otherwise
        """
        if self.is_running:
            logger.warning("Terminal %d is already running", self.terminal_id)
            return True

        try:
            if platform.system() == "Windows":
                return self._start_windows()
            else:
                return self._start_unix()
        except Exception as e:
            logger.error("Failed to start terminal %d: %s", self.terminal_id, e)
            return False

    def _start_windows(self) -> bool:
        """Start PTY process on Windows using pywinpty.

        Returns:
            True if started successfully, False otherwise
        """
        try:
            # Import pywinpty - only available on Windows
            from winpty import PtyProcess  # type: ignore[import-not-found]

            # Detect shell - prefer git-bash, fallback to cmd
            shell = self._get_windows_shell()
            logger.debug("Using shell for terminal %d: %s", self.terminal_id, shell)

            # Start PTY process - pywinpty has incomplete type stubs
            self._pty_process = PtyProcess.spawn(  # type: ignore[reportUnknownMemberType]
                [shell],
                dimensions=(self._rows, self._cols),
                cwd=self.cwd,
            )
            self.is_running = True
            logger.info("Started Windows terminal %d with %s", self.terminal_id, shell)
            return True

        except ImportError as e:
            logger.error("pywinpty not available: %s", e)
            return False
        except Exception as e:
            logger.error("Failed to start Windows terminal %d: %s", self.terminal_id, e)
            return False

    def _start_unix(self) -> bool:
        """Start PTY process on Unix using the pty module.

        Returns:
            True if started successfully, False otherwise
        """
        try:
            import pty

            # Get user's shell
            shell = os.environ.get("SHELL", "/bin/sh")

            # Create PTY - pty.fork() is Unix-only
            pid, fd = pty.fork()  # type: ignore[attr-defined]
            if pid == 0:
                # Child process
                os.chdir(self.cwd)
                os.execvp(shell, [shell])
                # If execvp fails, exit (unreachable if execvp succeeds)
                sys.exit(1)  # pragma: no cover
            else:
                # Parent process
                self._pty_fd = fd
                self._pty_process = pid
                self.is_running = True
                logger.info("Started Unix terminal %d with %s (pid=%d)", self.terminal_id, shell, pid)
                return True

        except Exception as e:
            logger.error("Failed to start Unix terminal %d: %s", self.terminal_id, e)
            return False

    def _get_windows_shell(self) -> str:
        """Detect the best shell to use on Windows.

        Prefers git-bash if available, otherwise uses cmd.exe.

        Returns:
            Path to shell executable
        """
        from clud.util import detect_git_bash

        git_bash = detect_git_bash()
        if git_bash:
            return git_bash

        # Fallback to cmd.exe
        return os.environ.get("COMSPEC", "cmd.exe")

    def stop(self) -> None:
        """Stop the PTY process and clean up resources."""
        if not self.is_running:
            return

        try:
            # Cancel read task if running
            if self._read_task and not self._read_task.done():
                self._read_task.cancel()
                self._read_task = None

            if platform.system() == "Windows":
                self._stop_windows()
            else:
                self._stop_unix()

        except Exception as e:
            logger.error("Error stopping terminal %d: %s", self.terminal_id, e)
        finally:
            self.is_running = False
            self._pty_process = None
            self._pty_fd = None
            self._websocket = None

    def _stop_windows(self) -> None:
        """Stop Windows PTY process."""
        if self._pty_process is not None:
            try:
                from winpty import PtyProcess  # type: ignore[import-not-found]

                if isinstance(self._pty_process, PtyProcess):
                    self._pty_process.terminate()
            except Exception as e:
                logger.warning("Error terminating Windows PTY %d: %s", self.terminal_id, e)

    def _stop_unix(self) -> None:
        """Stop Unix PTY process."""
        import signal

        # Close file descriptor
        if self._pty_fd is not None:
            with contextlib.suppress(OSError):
                os.close(self._pty_fd)

        # Kill child process
        if self._pty_process is not None and isinstance(self._pty_process, int):
            try:
                os.kill(self._pty_process, signal.SIGTERM)
                # Wait briefly for clean termination - WNOHANG is Unix-only
                os.waitpid(self._pty_process, os.WNOHANG)  # type: ignore[attr-defined]
            except (OSError, ChildProcessError):
                pass

    async def handle_websocket(self, websocket: Any) -> None:
        """Handle WebSocket connection for this terminal.

        Bridges the WebSocket connection with the PTY process, forwarding
        input/output in both directions.

        Args:
            websocket: WebSocket connection to the client
        """
        self._websocket = websocket
        logger.info("WebSocket connected for terminal %d", self.terminal_id)

        try:
            # Start PTY output reader
            self._read_task = asyncio.create_task(self._read_pty_output())

            # Process incoming WebSocket messages
            async for message in websocket:
                await self._handle_ws_message(message)

        except asyncio.CancelledError:
            logger.debug("WebSocket handler cancelled for terminal %d", self.terminal_id)
        except Exception as e:
            logger.error("WebSocket error for terminal %d: %s", self.terminal_id, e)
        finally:
            # Clean up reader task
            if self._read_task and not self._read_task.done():
                self._read_task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await self._read_task
            self._websocket = None
            logger.info("WebSocket disconnected for terminal %d", self.terminal_id)

    async def _handle_ws_message(self, message: str | bytes) -> None:
        """Handle an incoming WebSocket message.

        Messages can be either:
        - JSON resize messages: {"type": "resize", "cols": int, "rows": int}
        - Raw terminal input (string or bytes)

        Args:
            message: The message from the WebSocket
        """
        if isinstance(message, str):
            # Try to parse as JSON (resize command)
            try:
                data = json.loads(message)
                if isinstance(data, dict):
                    payload = cast(dict[str, object], data)
                    if payload.get("type") != "resize":
                        raise json.JSONDecodeError("not a resize control message", message, 0)
                    cols_value = payload.get("cols", 80)
                    rows_value = payload.get("rows", 24)
                    cols = int(cols_value) if isinstance(cols_value, (int, str)) else 80
                    rows = int(rows_value) if isinstance(rows_value, (int, str)) else 24
                    await self._resize(cols, rows)
                    return
            except json.JSONDecodeError:
                pass

            # Regular input - strip terminal capability response sequences before forwarding.
            filtered_message = self._input_filter.filter_terminal_responses(message)
            if filtered_message:
                self._input_tracker.observe(filtered_message)
                await self._write_to_pty(filtered_message.encode("utf-8"))
        else:
            # Binary data - send directly
            await self._write_to_pty(message)

    def get_input_snapshot(self) -> InputSnapshot:
        """Return the latest tracked user draft state."""
        return self._input_tracker.snapshot()

    async def inject_hook_failure(
        self,
        failure_path: str,
        *,
        instructions: str | None = None,
    ) -> bool:
        """Inject a Codex-facing hook failure notice and restore draft input.

        Returns True when the draft buffer was considered reliable enough to
        clear and restore safely. When False, callers should fall back to plain
        terminal output instead of rewriting the live input buffer.
        """
        snapshot = self._input_tracker.snapshot()
        if not snapshot.reliable:
            return False

        notice = instructions or f"Post-edit hook failed. Read {failure_path}. Delete it when finished, then continue."
        if snapshot.draft:
            await self._send_synthetic_input("\x15")
        await self._send_synthetic_input(notice + "\r")
        if snapshot.draft:
            await self._send_synthetic_input(snapshot.draft)
        return True

    async def _resize(self, cols: int, rows: int) -> None:
        """Resize the PTY terminal.

        Args:
            cols: New column count
            rows: New row count
        """
        self._cols = cols
        self._rows = rows

        if platform.system() == "Windows":
            await self._resize_windows(cols, rows)
        else:
            await self._resize_unix(cols, rows)

    async def _resize_windows(self, cols: int, rows: int) -> None:
        """Resize Windows PTY."""
        try:
            if self._pty_process is not None:
                from winpty import PtyProcess  # type: ignore[import-not-found]

                if isinstance(self._pty_process, PtyProcess):
                    self._pty_process.setwinsize(rows, cols)  # type: ignore[reportUnknownMemberType]
                    logger.debug("Resized Windows terminal %d to %dx%d", self.terminal_id, cols, rows)
        except Exception as e:
            logger.warning("Failed to resize Windows terminal %d: %s", self.terminal_id, e)

    async def _resize_unix(self, cols: int, rows: int) -> None:
        """Resize Unix PTY using TIOCSWINSZ."""
        try:
            import fcntl
            import struct
            import termios

            if self._pty_fd is not None:
                winsize = struct.pack("HHHH", rows, cols, 0, 0)
                # fcntl.ioctl and termios.TIOCSWINSZ are Unix-only
                fcntl.ioctl(self._pty_fd, termios.TIOCSWINSZ, winsize)  # type: ignore[attr-defined]
                logger.debug("Resized Unix terminal %d to %dx%d", self.terminal_id, cols, rows)
        except Exception as e:
            logger.warning("Failed to resize Unix terminal %d: %s", self.terminal_id, e)

    async def _write_to_pty(self, data: bytes) -> None:
        """Write data to the PTY input.

        Args:
            data: Bytes to write to PTY stdin
        """
        if not self.is_running:
            return

        try:
            if platform.system() == "Windows":
                await self._write_to_pty_windows(data)
            else:
                await self._write_to_pty_unix(data)
        except Exception as e:
            logger.error("Error writing to PTY %d: %s", self.terminal_id, e)

    async def _send_synthetic_input(self, data: str) -> None:
        """Send internally generated input while keeping draft tracking aligned."""
        self._input_tracker.observe(data)
        await self._write_to_pty(data.encode("utf-8"))

    async def _write_to_pty_windows(self, data: bytes) -> None:
        """Write data to Windows PTY."""
        if self._pty_process is not None:
            from winpty import PtyProcess  # type: ignore[import-not-found]

            if isinstance(self._pty_process, PtyProcess):
                # Run in executor to avoid blocking - pywinpty has incomplete type stubs
                loop = asyncio.get_event_loop()
                write_func: Any = self._pty_process.write  # type: ignore[reportUnknownMemberType]
                await loop.run_in_executor(None, write_func, data.decode("utf-8", errors="replace"))

    async def _write_to_pty_unix(self, data: bytes) -> None:
        """Write data to Unix PTY."""
        if self._pty_fd is not None:
            loop = asyncio.get_event_loop()
            await loop.run_in_executor(None, os.write, self._pty_fd, data)

    async def _read_pty_output(self) -> None:
        """Read output from PTY and send to WebSocket.

        Continuously reads from the PTY and forwards output to the connected
        WebSocket client.
        """
        try:
            while self.is_running and self._websocket:
                if platform.system() == "Windows":
                    data = await self._read_from_pty_windows()
                else:
                    data = await self._read_from_pty_unix()

                if data and self._websocket:
                    await self._websocket.send(data)

        except asyncio.CancelledError:
            raise
        except Exception as e:
            if self.is_running:
                logger.error("Error reading from PTY %d: %s", self.terminal_id, e)

    async def _read_from_pty_windows(self) -> bytes | None:
        """Read data from Windows PTY.

        Returns:
            Bytes read from PTY, or None if no data available
        """
        if self._pty_process is None:
            return None

        try:
            from winpty import PtyProcess  # type: ignore[import-not-found]

            if isinstance(self._pty_process, PtyProcess):
                loop = asyncio.get_event_loop()
                # Use a moderate timeout to allow shell processing while
                # still being responsive to is_running checks
                data = await asyncio.wait_for(
                    loop.run_in_executor(None, self._pty_process.read, 4096),
                    timeout=0.5,
                )
                if data:
                    return data.encode("utf-8", errors="replace")
        except asyncio.TimeoutError:
            pass
        except Exception as e:
            if self.is_running:
                logger.debug("Read error from Windows PTY %d: %s", self.terminal_id, e)
        return None

    async def _read_from_pty_unix(self) -> bytes | None:
        """Read data from Unix PTY.

        Returns:
            Bytes read from PTY, or None if no data available
        """
        if self._pty_fd is None:
            return None

        try:
            loop = asyncio.get_event_loop()
            # Use a moderate timeout to allow shell processing while
            # still being responsive to is_running checks
            data = await asyncio.wait_for(
                loop.run_in_executor(None, os.read, self._pty_fd, 4096),
                timeout=0.5,
            )
            return data if data else None
        except asyncio.TimeoutError:
            pass
        except Exception as e:
            if self.is_running:
                logger.debug("Read error from Unix PTY %d: %s", self.terminal_id, e)
        return None


class TerminalManager:
    """Manages multiple terminal sessions.

    Coordinates creation, lifecycle, and access to multiple Terminal instances.

    Attributes:
        num_terminals: Number of terminals being managed
        terminals: Dictionary mapping terminal IDs to Terminal instances
    """

    def __init__(self, num_terminals: int = 8, cwd: str | None = None) -> None:
        """Initialize the terminal manager.

        Args:
            num_terminals: Number of terminals to manage (default 8)
            cwd: Working directory for all terminals. Defaults to user's home.
        """
        self.num_terminals = num_terminals
        self.cwd = cwd or str(Path.home())
        self.terminals: dict[int, Terminal] = {}

    def start_all(self) -> int:
        """Start all terminal sessions.

        Creates and starts PTY processes for all terminals.

        Returns:
            Number of terminals successfully started
        """
        started = 0
        for i in range(self.num_terminals):
            terminal = Terminal(terminal_id=i, cwd=self.cwd)
            if terminal.start():
                self.terminals[i] = terminal
                started += 1
            else:
                logger.error("Failed to start terminal %d", i)

        logger.info("Started %d/%d terminals", started, self.num_terminals)
        return started

    def stop_all(self) -> None:
        """Stop all terminal sessions.

        Terminates all PTY processes and cleans up resources.
        """
        for terminal_id, terminal in list(self.terminals.items()):
            try:
                terminal.stop()
                logger.debug("Stopped terminal %d", terminal_id)
            except Exception as e:
                logger.error("Error stopping terminal %d: %s", terminal_id, e)

        self.terminals.clear()
        logger.info("Stopped all terminals")

    def get_terminal(self, terminal_id: int) -> Terminal | None:
        """Get a terminal by its ID.

        Args:
            terminal_id: The terminal ID to look up

        Returns:
            The Terminal instance, or None if not found
        """
        return self.terminals.get(terminal_id)

    def is_all_running(self) -> bool:
        """Check if all terminals are running.

        Returns:
            True if all terminals are running, False otherwise
        """
        if len(self.terminals) != self.num_terminals:
            return False
        return all(t.is_running for t in self.terminals.values())

    def get_running_count(self) -> int:
        """Get the number of running terminals.

        Returns:
            Count of terminals currently running
        """
        return sum(1 for t in self.terminals.values() if t.is_running)
