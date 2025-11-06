"""Process management utilities for launching detached processes."""

import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path


def launch_detached(command: Sequence[str | Path]) -> None:
    """
    Launch a command in a detached process that doesn't block the terminal.

    This function starts a process that runs completely independently of the parent
    terminal. The terminal returns control immediately, and the process continues
    running even if the terminal is closed.

    Cross-platform implementation:
    - Windows: Uses DETACHED_PROCESS and CREATE_NEW_PROCESS_GROUP flags
    - Unix-like: Uses start_new_session=True

    Args:
        command: List of command arguments (e.g., ["sublime_text.exe", "file.txt"])
                 Can contain strings or Path objects, which will be converted to strings

    Example:
        >>> launch_detached(["notepad.exe", "file.txt"])
        >>> # Terminal is immediately ready for next command
        >>> # Notepad runs independently

    Notes:
        - All stdio (stdin, stdout, stderr) is redirected to DEVNULL
        - Process is completely detached from the parent's terminal
        - No zombie processes - proper process management with Popen
        - Works across Windows, macOS, and Linux
    """
    # Convert Path objects to strings
    cmd = [str(arg) for arg in command]

    if sys.platform == "win32":
        # Windows: Use DETACHED_PROCESS to create a process with no console attached
        # and CREATE_NEW_PROCESS_GROUP to prevent Ctrl+C from propagating
        DETACHED_PROCESS = 0x00000008
        CREATE_NEW_PROCESS_GROUP = 0x00000200

        subprocess.Popen(
            cmd,
            creationflags=DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
        )
    else:
        # Unix-like systems: Use start_new_session to create a new session
        # with the process as the session leader, detaching from the terminal
        subprocess.Popen(
            cmd,
            start_new_session=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
        )
