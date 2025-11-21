"""Claude Code executable discovery utilities.

This module provides functions to locate the Claude Code executable
in the system PATH or local installation.
"""

from clud.claude_installer import find_claude_code


def _find_claude_path() -> str | None:
    """Find the path to the Claude executable.

    This is now a wrapper around claude_installer.find_claude_code()
    for backward compatibility.
    """
    return find_claude_code()
