"""
Configuration directory and file management.

This module handles configuration file I/O operations, including
directory creation.
"""

from pathlib import Path


def get_clud_config_dir() -> Path:
    """Get or create the .clud config directory."""
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(exist_ok=True)
    return config_dir
