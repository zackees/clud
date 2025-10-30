"""
Git pre-check functionality using the CodeUp API.

This module provides a wrapper around the CodeUp API's pre_check_git() method
to detect git repository changes and optionally handle untracked files interactively.
"""

import sys
from typing import Any, NamedTuple

# Try to import Codeup, but handle gracefully if not available
try:
    from codeup import Codeup  # type: ignore[import-untyped]

    CODEUP_AVAILABLE = True
except ImportError:
    CODEUP_AVAILABLE = False
    Codeup: Any = None


class GitPreCheckResult(NamedTuple):
    """Result of a git pre-check operation."""

    success: bool
    error_message: str
    has_changes: bool
    untracked_files: list[str]
    staged_files: list[str]
    unstaged_files: list[str]


def run_git_precheck(allow_interactive: bool = False) -> GitPreCheckResult:
    """
    Run git pre-check using the CodeUp API.

    This function wraps the CodeUp API's pre_check_git() method and converts
    the result to a GitPreCheckResult for easier consumption.

    Args:
        allow_interactive: If True, prompts user to add untracked files interactively.
                          Requires a PTY (terminal). If False, only checks status.

    Returns:
        GitPreCheckResult with success status and detected changes.

    Example:
        >>> result = run_git_precheck(allow_interactive=False)
        >>> if result.success and result.has_changes:
        ...     print(f"Found {len(result.untracked_files)} untracked files")
    """
    if not CODEUP_AVAILABLE or Codeup is None:
        return GitPreCheckResult(
            success=False,
            error_message="CodeUp package is not installed. Install with: pip install codeup>=1.0.22",
            has_changes=False,
            untracked_files=[],
            staged_files=[],
            unstaged_files=[],
        )

    result = Codeup.pre_check_git(allow_interactive=allow_interactive)

    return GitPreCheckResult(
        success=result.success,
        error_message=result.error_message or "",
        has_changes=result.has_changes,
        untracked_files=result.untracked_files,
        staged_files=result.staged_files,
        unstaged_files=result.unstaged_files,
    )


def run_two_phase_precheck(verbose: bool = True) -> GitPreCheckResult:
    """
    Run two-phase git pre-check: non-interactive check followed by interactive handling.

    This implements the recommended workflow from the CodeUp API documentation:
    1. Phase 1: Non-interactive check to detect changes
    2. Phase 2: If untracked files exist, run interactive mode to handle them

    Args:
        verbose: If True, prints status messages to stdout

    Returns:
        GitPreCheckResult with the final status after both phases.

    Example:
        >>> result = run_two_phase_precheck()
        >>> if result.success and not result.has_changes:
        ...     print("Repository is clean")
    """
    # Phase 1: Non-interactive check
    if verbose:
        print("Checking git status...")

    result = run_git_precheck(allow_interactive=False)

    if not result.success:
        if verbose:
            print(f"Error during git check: {result.error_message}")
        return result

    # Clean repository - no changes
    if not result.has_changes:
        if verbose:
            print("✓ No changes detected - repository is clean")
        return result

    # Display summary of changes
    if verbose:
        print("\n📋 Changes detected:")
        print(f"  • Untracked files: {len(result.untracked_files)}")
        print(f"  • Staged files: {len(result.staged_files)}")
        print(f"  • Unstaged files: {len(result.unstaged_files)}")

    # Phase 2: Handle untracked files if present
    if result.untracked_files:
        # Check if we can run interactive mode
        if not sys.stdin.isatty():
            if verbose:
                print("\n⚠️  Cannot run interactive mode (no TTY)")
                print("   Please run this interactively or add files manually")
            return result

        if verbose:
            print("\n🔄 Running interactive file add...")

        interactive_result = run_git_precheck(allow_interactive=True)

        if not interactive_result.success:
            if verbose:
                print(f"Error during interactive add: {interactive_result.error_message}")
            return interactive_result

        if verbose:
            print("✓ Interactive file add completed")
        return interactive_result
    else:
        if verbose:
            print("✓ No untracked files - ready to proceed")
        return result


def display_git_status(result: GitPreCheckResult, max_files: int = 5) -> None:
    """
    Display a formatted summary of git status.

    Args:
        result: The GitPreCheckResult to display
        max_files: Maximum number of files to show per category (default: 5)
    """
    if not result.success:
        print(f"❌ Git check failed: {result.error_message}")
        return

    if not result.has_changes:
        print("✓ Clean repository - no changes")
        return

    print("Git Status:")

    if result.untracked_files:
        print(f"\n  Untracked ({len(result.untracked_files)}):")
        for f in result.untracked_files[:max_files]:
            print(f"    - {f}")
        if len(result.untracked_files) > max_files:
            print(f"    ... and {len(result.untracked_files) - max_files} more")

    if result.staged_files:
        print(f"\n  Staged ({len(result.staged_files)}):")
        for f in result.staged_files[:max_files]:
            print(f"    - {f}")
        if len(result.staged_files) > max_files:
            print(f"    ... and {len(result.staged_files) - max_files} more")

    if result.unstaged_files:
        print(f"\n  Unstaged ({len(result.unstaged_files)}):")
        for f in result.unstaged_files[:max_files]:
            print(f"    - {f}")
        if len(result.unstaged_files) > max_files:
            print(f"    ... and {len(result.unstaged_files) - max_files} more")
