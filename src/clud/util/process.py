"""Process management utilities for launching detached or isolated processes."""

import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

from running_process import IdleWaitResult, RunningProcess


def _normalize_running_process_command(
    command: str | Sequence[str | Path],
    *,
    shell: bool,
) -> tuple[str | list[str], bool]:
    """Adapt Windows command shims for RunningProcess 3.x.

    Windows `.cmd` and `.bat` launchers are shell scripts rather than native
    executables. Running them directly now raises `%1 is not a valid Win32
    application`, so route those invocations through the shell while preserving
    normal direct execution for real binaries.
    """
    normalized_command: str | list[str] = command if isinstance(command, str) else [str(part) for part in command]
    if shell or sys.platform != "win32" or isinstance(normalized_command, str) or not normalized_command:
        return normalized_command, shell

    launcher = normalized_command[0].lower()
    if launcher.endswith((".cmd", ".bat")):
        return subprocess.list2cmdline(normalized_command), True

    return normalized_command, shell


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


def run_with_input_detached(
    command: str | Sequence[str | Path],
    *,
    shell: bool = False,
    cwd: str | Path | None = None,
    env: dict[str, str] | None = None,
    text: bool = True,
    capture_output: bool = True,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a command while isolating it from the parent terminal's stdin.

    This is intended for helper subprocesses such as hooks that should not be
    able to consume or block on the user's interactive terminal input. Output
    capture goes through ``RunningProcess.run()`` so stdout is continuously
    drained and large outputs do not deadlock on pipe buffers.
    """
    normalized_command, effective_shell = _normalize_running_process_command(command, shell=shell)

    process = RunningProcess(
        command=normalized_command,
        cwd=Path(cwd) if cwd is not None else None,
        check=False,
        auto_run=True,
        shell=effective_shell,
        timeout=int(timeout) if timeout is not None else None,
        env=env,
        stdin=subprocess.DEVNULL,
    )
    wait_result = process.wait()
    returncode: int = (wait_result.returncode or 0) if isinstance(wait_result, IdleWaitResult) else wait_result
    raw_output = process.combined_output if capture_output else None
    stdout: str | None = str(raw_output) if isinstance(raw_output, bytes) else raw_output
    return subprocess.CompletedProcess(
        args=normalized_command,
        returncode=returncode,
        stdout=stdout,
        stderr=None,
    )


def run_captured(
    command: str | Sequence[str | Path],
    *,
    shell: bool = False,
    cwd: str | Path | None = None,
    env: dict[str, str] | None = None,
    text: bool = True,
    timeout: float | None = None,
    check: bool = False,
    encoding: str | None = None,
    errors: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a command with robust captured output via RunningProcess.run()."""
    normalized_command, effective_shell = _normalize_running_process_command(command, shell=shell)

    process = RunningProcess(
        command=normalized_command,
        cwd=Path(cwd) if cwd is not None else None,
        check=False,
        auto_run=True,
        shell=effective_shell,
        timeout=int(timeout) if timeout is not None else None,
        env=env,
    )
    wait_result = process.wait()
    returncode: int = (wait_result.returncode or 0) if isinstance(wait_result, IdleWaitResult) else wait_result
    raw_stdout = process.stdout
    stdout: str | None = str(raw_stdout) if isinstance(raw_stdout, bytes) else raw_stdout
    completed: subprocess.CompletedProcess[str] = subprocess.CompletedProcess(
        args=normalized_command,
        returncode=returncode,
        stdout=stdout,
        stderr=None,
    )
    if check and returncode != 0:
        raise subprocess.CalledProcessError(
            returncode=returncode,
            cmd=normalized_command,
            output=completed.stdout,
            stderr=None,
        )
    return completed
