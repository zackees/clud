"""Agent task management utilities.

This module provides functions for managing AGENT.task.md files and
handling existing agent sessions.
"""

import re
import shutil
import sys
import time
from pathlib import Path


def _handle_existing_agent_task(agent_task_dir: Path) -> tuple[bool, int]:
    """Handle existing .agent_task directory from previous session.

    Args:
        agent_task_dir: Path to .agent_task directory

    Returns:
        Tuple of (should_continue, start_iteration)
        - should_continue: False if user cancelled
        - start_iteration: Iteration number to start from (1 for fresh, N+1 for continuation)
    """
    if not agent_task_dir.exists():
        return True, 1

    # Scan for existing files
    iteration_files = sorted(agent_task_dir.glob("ITERATION_*.md"))

    # Check for DONE.md at project root (new location)
    done_file_root = Path("DONE.md")

    # If directory is empty and no root DONE.md, treat as fresh start
    if not iteration_files and not done_file_root.exists():
        return True, 1

    # Display warning
    print("\n⚠️  Previous agent session detected (.agent_task/ exists)", file=sys.stderr)
    print("Contains:", file=sys.stderr)

    for file in iteration_files:
        mtime = file.stat().st_mtime
        timestamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime))
        print(f"  - {file.name} ({timestamp})", file=sys.stderr)

    # Check for DONE.md at project root
    if done_file_root.exists():
        mtime = done_file_root.stat().st_mtime
        timestamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime))
        print(f"\n  - DONE.md at project root ({timestamp}) ⚠️  Will halt immediately!", file=sys.stderr)

    # Prompt user
    print(file=sys.stderr)
    sys.stdout.flush()

    try:
        response = input("Delete and start over? [y/n]: ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        print("\nOperation cancelled.", file=sys.stderr)
        return False, 1

    if response in ["y", "yes"]:
        # Delete entire directory
        try:
            shutil.rmtree(agent_task_dir)
            print("✓ Previous session deleted", file=sys.stderr)
            return True, 1
        except Exception as e:
            print(f"Error: Failed to delete .agent_task directory: {e}", file=sys.stderr)
            return False, 1

    elif response in ["n", "no"]:
        # Keep directory, determine next iteration
        last_iteration = 0
        for file in iteration_files:
            # Extract number from ITERATION_N.md
            match = re.match(r"ITERATION_(\d+)\.md", file.name)
            if match:
                last_iteration = max(last_iteration, int(match.group(1)))

        next_iteration = last_iteration + 1
        print(f"✓ Continuing from iteration {next_iteration}", file=sys.stderr)
        return True, next_iteration

    else:
        print("Invalid response. Operation cancelled.", file=sys.stderr)
        return False, 1


def _print_red_banner(message: str) -> None:
    """Print a red banner message to stderr for critical warnings."""
    terminal_width = shutil.get_terminal_size((80, 20)).columns
    banner_char = "="
    padding_char = " "

    # Build banner lines
    border: str = banner_char * terminal_width
    padding: str = padding_char * terminal_width

    # Center the message
    message_lines: list[str] = message.split("\n")
    centered_lines: list[str] = []
    for line in message_lines:
        spaces_needed = max(0, (terminal_width - len(line)) // 2)
        centered_line: str = padding_char * spaces_needed + line
        # Pad to full width
        centered_line = centered_line + padding_char * (terminal_width - len(centered_line))
        centered_lines.append(centered_line)

    # ANSI color codes: red background + white text
    RED_BG = "\033[41m"
    WHITE_TEXT = "\033[97m"
    BOLD = "\033[1m"
    RESET = "\033[0m"

    # Print banner
    print(file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{border}{RESET}", file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{padding}{RESET}", file=sys.stderr)
    for centered_line_text in centered_lines:
        print(f"{RED_BG}{WHITE_TEXT}{BOLD}{centered_line_text}{RESET}", file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{padding}{RESET}", file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{border}{RESET}", file=sys.stderr)
    print(file=sys.stderr)
