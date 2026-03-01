"""
GitHub URL handling for fix command.

This module provides utilities for detecting and processing GitHub URLs
in fix commands.
"""

from clud.agent.prompts import GITHUB_FIX_TEMPLATE, GITHUB_FIX_VALIDATION


def is_github_url(url: str) -> bool:
    """Check if the URL is a GitHub URL."""
    return url.startswith(("https://github.com/", "http://github.com/"))


def generate_github_fix_prompt(url: str) -> str:
    """Generate a prompt for fixing issues based on a GitHub URL."""
    return GITHUB_FIX_TEMPLATE.format(url=url, validation=GITHUB_FIX_VALIDATION)
