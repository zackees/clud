"""General utilities for clud."""

import logging
import socket
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
