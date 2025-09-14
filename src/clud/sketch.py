"""Sketch and project utilities for clud."""

from pathlib import Path


def looks_like_fastled_repo(path: Path) -> bool:
    """Check if a path looks like a FastLED repository."""
    # For clud, we'll adapt this to be more generic
    # Check for common project indicators
    if not path.exists() or not path.is_dir():
        return False

    # Check for common project files
    indicators = ["src", "lib", "include", "CMakeLists.txt", "Makefile", "pyproject.toml", "package.json"]

    found_indicators = sum(1 for indicator in indicators if (path / indicator).exists())

    # If we find multiple indicators, it's likely a project
    return found_indicators >= 2
