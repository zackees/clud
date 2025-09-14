"""Emoji utilities for clud CLI."""

import sys


def EMO(emoji: str, fallback: str) -> str:
    """Return emoji on supported platforms, fallback text otherwise."""
    # Simple check for emoji support - assume modern terminals support it
    # except on older Windows systems
    if sys.platform == "win32":
        # Check if we're in a modern terminal (Windows Terminal, etc.)
        try:
            # Try to encode emoji to see if it's supported
            emoji.encode("utf-8")
            return emoji
        except UnicodeEncodeError:
            return fallback
    return emoji
