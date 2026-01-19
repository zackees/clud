"""CLI handler for the multi-terminal UI command.

This module provides the command handler for `clud --ui`, which launches
a Playwright browser with multiple xterm.js terminals in a grid layout.

The UI runs in its own process space with its own signal handlers,
ensuring Ctrl+C in the client does not propagate to the UI process.
"""

from __future__ import annotations

import logging
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

logger = logging.getLogger(__name__)

# PID file location for tracking running daemon
_PID_FILE = Path.home() / ".clud" / "daemon.pid"
_LOG_FILE = Path.home() / ".clud" / "logs" / "daemon.log"


def handle_daemon_command(num_terminals: int = 4) -> int:
    """Handle the --ui command to launch multi-terminal UI.

    Spawns the UI as a separate process with its own signal handlers.
    The client monitors the UI process and exits when it stops.

    Args:
        num_terminals: Number of terminals to create (default 4)

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    home_dir = Path.home()

    print("Starting CLUD multi-terminal UI...")
    print(f"  {num_terminals} terminals will open in Playwright browser")
    print(f"  All terminals start in: {home_dir}")
    print()

    # Check if daemon is already running
    if _is_daemon_running():
        pid = _read_pid()
        print(f"Daemon is already running (PID: {pid})")
        print(f"Stop it first with: taskkill /F /PID {pid}" if sys.platform == "win32" else f"Stop it first with: kill {pid}")
        return 1

    # Clean up stale PID file
    if _PID_FILE.exists():
        logger.info("Cleaning up stale PID file")
        _PID_FILE.unlink()

    # Ensure log directory exists
    _LOG_FILE.parent.mkdir(parents=True, exist_ok=True)

    # Start daemon as background process
    daemon_pid = _start_daemon_process(num_terminals)
    if daemon_pid is None:
        print("Error: Failed to start daemon process", file=sys.stderr)
        return 1

    # Wait briefly for daemon to initialize
    time.sleep(1.0)

    # Verify daemon is running
    if not _is_daemon_running():
        print("Error: Daemon failed to start. Check logs at:", file=sys.stderr)
        print(f"  {_LOG_FILE}", file=sys.stderr)
        return 1

    print(f"Daemon started (PID: {daemon_pid})")
    print("Close the browser window to stop the daemon.")
    print(f"Logs: {_LOG_FILE}")
    print()

    # Monitor daemon - wait for it to exit
    return _monitor_daemon(daemon_pid)


def _start_daemon_process(num_terminals: int) -> int | None:
    """Start the daemon as a background process.

    Args:
        num_terminals: Number of terminals to create

    Returns:
        PID of the daemon process, or None if failed
    """
    # Use sys.executable to get the current Python interpreter
    python_exe = sys.executable

    # On Windows, use pythonw.exe to prevent console windows
    if sys.platform == "win32":
        pythonw_exe = Path(sys.executable).parent / "pythonw.exe"
        if pythonw_exe.exists():
            python_exe = str(pythonw_exe)
            logger.info("Using pythonw.exe: %s", python_exe)

    cmd = [python_exe, "-m", "clud.daemon", "run", "--num-terminals", str(num_terminals)]

    logger.info("Starting daemon: %s", " ".join(cmd))

    try:
        if sys.platform == "win32":
            # Windows: Use CREATE_NO_WINDOW + CREATE_NEW_PROCESS_GROUP
            CREATE_NO_WINDOW = 0x08000000
            CREATE_NEW_PROCESS_GROUP = 0x00000200
            DETACHED_PROCESS = 0x00000008

            # Use DETACHED_PROCESS for pythonw.exe
            creation_flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS if "pythonw" in python_exe.lower() else CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP

            with open(_LOG_FILE, "a", encoding="utf-8") as log_f:
                process = subprocess.Popen(
                    cmd,
                    creationflags=creation_flags,
                    stdout=log_f,
                    stderr=log_f,
                    stdin=subprocess.DEVNULL,
                )
        else:
            # Unix: Use start_new_session to detach from parent
            with open(_LOG_FILE, "a", encoding="utf-8") as log_f:
                process = subprocess.Popen(
                    cmd,
                    start_new_session=True,
                    stdout=log_f,
                    stderr=log_f,
                    stdin=subprocess.DEVNULL,
                )

        # Write PID file
        _PID_FILE.parent.mkdir(parents=True, exist_ok=True)
        _PID_FILE.write_text(str(process.pid), encoding="utf-8")

        return process.pid

    except Exception as e:
        logger.exception("Failed to start daemon process: %s", e)
        return None


def _monitor_daemon(pid: int) -> int:
    """Monitor the daemon process and wait for it to exit.

    Handles Ctrl+C by sending SIGTERM to the daemon.

    Args:
        pid: PID of the daemon process

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        # Wait for daemon to exit
        while _is_process_running(pid):
            time.sleep(0.5)

        print("\nDaemon stopped.")
        return 0

    except KeyboardInterrupt:
        # User pressed Ctrl+C - send signal to daemon
        print("\nStopping daemon...")
        _stop_daemon(pid)
        return 0


def _stop_daemon(pid: int) -> None:
    """Stop the daemon process gracefully.

    Args:
        pid: PID of the daemon process
    """
    try:
        if sys.platform == "win32":
            # Windows: Use taskkill
            subprocess.run(
                ["taskkill", "/F", "/PID", str(pid)],
                check=False,
                capture_output=True,
            )
        else:
            # Unix: Send SIGTERM
            os.kill(pid, signal.SIGTERM)

        # Wait for process to exit
        max_wait = 5.0
        waited = 0.0
        while waited < max_wait and _is_process_running(pid):
            time.sleep(0.1)
            waited += 0.1

        # Force kill if still running
        if _is_process_running(pid):
            logger.warning("Daemon did not stop gracefully, forcing kill")
            if sys.platform == "win32":
                subprocess.run(
                    ["taskkill", "/F", "/PID", str(pid)],
                    check=False,
                    capture_output=True,
                )
            else:
                os.kill(pid, signal.SIGKILL)

    except (ProcessLookupError, PermissionError, OSError) as e:
        logger.debug("Error stopping daemon: %s", e)

    finally:
        # Clean up PID file
        _PID_FILE.unlink(missing_ok=True)


def _is_daemon_running() -> bool:
    """Check if the daemon is currently running.

    Returns:
        True if daemon is running, False otherwise
    """
    pid = _read_pid()
    if pid is None:
        return False
    return _is_process_running(pid)


def _read_pid() -> int | None:
    """Read PID from PID file.

    Returns:
        PID as integer, or None if file doesn't exist or is invalid
    """
    if not _PID_FILE.exists():
        return None

    try:
        pid_text = _PID_FILE.read_text(encoding="utf-8").strip()
        return int(pid_text)
    except (ValueError, OSError):
        return None


def _is_process_running(pid: int) -> bool:
    """Check if a process with given PID is running.

    Args:
        pid: Process ID to check

    Returns:
        True if process is running, False otherwise
    """
    try:
        if sys.platform == "win32":
            # Windows: Use tasklist
            result = subprocess.run(
                ["tasklist", "/FI", f"PID eq {pid}"],
                capture_output=True,
                text=True,
                check=False,
            )
            return str(pid) in result.stdout
        else:
            # Unix: Send signal 0 to check if process exists
            os.kill(pid, 0)
            return True
    except (ProcessLookupError, PermissionError, OSError):
        return False
