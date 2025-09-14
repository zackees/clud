"""General utilities for clud."""

import socket
from pathlib import Path


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
