"""General utilities for clud."""

import logging
import os
import platform
import socket
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import TypeVar

T = TypeVar("T")


def port_is_free(port: int, host: str = "localhost") -> bool:
    """Check if a port is free on the given host."""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(1)
            result = sock.connect_ex((host, port))
            return result != 0
    except Exception:
        return False


def print_banner(message: str, char: str = "=") -> None:
    """Print a banner message with decorative characters."""
    length = len(message) + 4
    border = char * length
    print(border)
    print(f"{char} {message} {char}")
    print(border)


def download_emsdk_headers(url: str, filepath: Path) -> str | None:
    """Placeholder for downloading EMSDK headers."""
    # This is a placeholder - implement as needed for clud
    return None


def handle_keyboard_interrupt(
    func: Callable[..., T],
    *args: object,
    cleanup: Callable[[], None] | None = None,
    logger: logging.Logger | None = None,
    log_message: str | None = None,
    **kwargs: object,
) -> T:
    """Execute a function with proper KeyboardInterrupt handling.

    This utility ensures KeyboardInterrupt is ALWAYS re-raised immediately,
    preventing unresponsive processes. It provides optional cleanup and logging.

    Args:
        func: The function to execute
        *args: Positional arguments to pass to func
        cleanup: Optional cleanup function to call before re-raising KeyboardInterrupt
        logger: Optional logger for logging the interrupt
        log_message: Optional custom log message (default: "Operation interrupted by user")
        **kwargs: Keyword arguments to pass to func

    Returns:
        The return value of func

    Raises:
        KeyboardInterrupt: Always re-raised when caught
        Exception: Any other exception from func is propagated

    Example:
        >>> def risky_operation(x: int) -> int:
        ...     return x * 2
        >>> result = handle_keyboard_interrupt(risky_operation, 5)
        >>> # If user presses Ctrl+C during execution, KeyboardInterrupt is re-raised
        >>> # If operation completes, result is returned (10)

        >>> # With cleanup:
        >>> def cleanup_resources() -> None:
        ...     print("Cleaning up...")
        >>> result = handle_keyboard_interrupt(
        ...     risky_operation, 5, cleanup=cleanup_resources
        ... )
    """
    try:
        return func(*args, **kwargs)
    except KeyboardInterrupt:
        # Always re-raise KeyboardInterrupt - CRITICAL for responsive Ctrl+C
        if cleanup:
            try:
                cleanup()
            except Exception as cleanup_err:
                # Don't let cleanup errors prevent KeyboardInterrupt propagation
                if logger:
                    logger.warning("Cleanup failed during keyboard interrupt: %s", cleanup_err)

        if logger:
            msg = log_message or "Operation interrupted by user"
            logger.info(msg)

        raise  # MANDATORY: Always re-raise KeyboardInterrupt


def detect_git_bash() -> str | None:
    """Detect git-bash on Windows.

    This function attempts to locate git-bash on Windows systems by:
    1. Using 'where bash' and 'where git-bash' to find candidates
    2. Validating each candidate with '--version' to ensure it's git-bash (not WSL)
    3. Checking common installation paths as fallback

    Returns:
        Path to git-bash executable if found, None otherwise.
        Always returns None on non-Windows systems.

    Note:
        This function specifically avoids WSL bash to prevent launching into
        a different OS environment. It only returns paths to native Windows
        git-bash installations.

    Example:
        >>> git_bash_path = detect_git_bash()
        >>> if git_bash_path:
        ...     print(f"Found git-bash at: {git_bash_path}")
        ... else:
        ...     print("git-bash not found")
    """
    # Only run on Windows
    if platform.system() != "Windows":
        return None

    candidates: list[str] = []

    # Try 'where bash' to find bash executables
    try:
        result = subprocess.run(
            ["where", "bash"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if result.returncode == 0:
            # 'where' returns multiple paths separated by newlines
            paths = result.stdout.strip().split("\n")
            candidates.extend(p.strip() for p in paths if p.strip())
    except (subprocess.SubprocessError, FileNotFoundError):
        pass

    # Try 'where git-bash' to find git-bash specifically
    try:
        result = subprocess.run(
            ["where", "git-bash"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if result.returncode == 0:
            paths = result.stdout.strip().split("\n")
            candidates.extend(p.strip() for p in paths if p.strip())
    except (subprocess.SubprocessError, FileNotFoundError):
        pass

    # Add common installation paths as fallback
    common_paths = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
        os.path.expandvars(r"%LOCALAPPDATA%\Programs\Git\bin\bash.exe"),
    ]
    candidates.extend(common_paths)

    # Validate each candidate
    for candidate_path in candidates:
        if not os.path.isfile(candidate_path):
            continue

        # Check if this is actually git-bash (not WSL)
        if _is_git_bash(candidate_path):
            return candidate_path

    return None


def _is_git_bash(bash_path: str) -> bool:
    """Validate that a bash executable is git-bash (not WSL).

    Args:
        bash_path: Path to bash executable to validate

    Returns:
        True if this is git-bash, False otherwise

    Note:
        This function checks:
        1. The executable can run '--version' successfully
        2. The version output contains "pc-msys" or "pc-windows-gnu" (git-bash signature)
        3. The path doesn't contain WSL indicators (wsl, lxss, AppData/Local/Packages)
    """
    # Check path for WSL indicators
    path_lower = bash_path.lower()
    wsl_indicators = ["wsl", "lxss", r"appdata\local\packages"]
    if any(indicator in path_lower for indicator in wsl_indicators):
        return False

    # Try to run bash --version
    try:
        result = subprocess.run(
            [bash_path, "--version"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if result.returncode != 0:
            return False

        version_output = result.stdout.lower()

        # Git-bash typically contains "pc-msys" or "pc-windows-gnu" in version
        git_bash_indicators = ["pc-msys", "pc-windows-gnu", "git for windows"]
        if any(indicator in version_output for indicator in git_bash_indicators):
            return True

        # If version output contains "linux" but not git-bash indicators, it's WSL
        if "linux" in version_output:
            return False

    except (subprocess.SubprocessError, FileNotFoundError, OSError):
        return False

    return False
