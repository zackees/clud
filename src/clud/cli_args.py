#!/usr/bin/env python3
"""Simplified CLI routing for clud."""

import sys
from dataclasses import dataclass
from enum import Enum


class AgentMode(Enum):
    """Agent execution modes."""

    FOREGROUND = "fg"
    BACKGROUND = "bg"
    FIX = "fix"
    UP = "up"


@dataclass
class RouterArgs:
    """Simple routing arguments for CLI."""

    mode: AgentMode
    remaining_args: list[str]
    # Commands that don't need agents
    login: bool = False
    task: str | None = None
    lint: bool = False
    test: bool = False
    fix: bool = False
    fix_url: str | None = None
    codeup: bool = False
    codeup_publish: bool = False
    kanban: bool = False
    help: bool = False


def parse_router_args(args: list[str] | None = None) -> RouterArgs:
    """Parse CLI arguments to determine routing mode and extract special commands."""
    if args is None:
        args = sys.argv[1:]

    # Create a copy to modify
    args_copy = args[:]
    mode = AgentMode.FOREGROUND  # Default to foreground

    # Check for special commands first (these don't need agent routing)
    login = "--login" in args_copy
    lint = "--lint" in args_copy
    test = "--test" in args_copy
    fix = "--fix" in args_copy  # --fix should be passed to agents, not intercepted
    codeup = "--codeup" in args_copy
    codeup_publish = "--codeup-publish" in args_copy or "--codeup-p" in args_copy
    kanban = "--kanban" in args_copy

    # Extract fix URL argument if present
    fix_url = None
    if "--fix" in args_copy:
        fix_idx = args_copy.index("--fix")
        # Check if there's a URL argument after --fix
        if fix_idx + 1 < len(args_copy) and not args_copy[fix_idx + 1].startswith("-"):
            fix_url = args_copy[fix_idx + 1]

    # Only intercept help if no mode is specified (i.e., generic help)
    has_mode_specified = (args_copy and args_copy[0] in ["bg", "fg"]) or "--bg" in args_copy
    help_requested = ("--help" in args_copy or "-h" in args_copy) and not has_mode_specified

    # Extract task argument if present
    task = None
    if "--task" in args_copy or "-t" in args_copy:
        task_flag = "--task" if "--task" in args_copy else "-t"
        task_idx = args_copy.index(task_flag)
        if task_idx + 1 < len(args_copy):
            task = args_copy[task_idx + 1]
            # Remove both the flag and its value from args_copy
            args_copy.pop(task_idx)  # Remove flag
            args_copy.pop(task_idx)  # Remove value (now at same index)
        else:
            # Flag present but no value - remove flag and set empty string to trigger error
            args_copy.pop(task_idx)
            task = ""  # Empty string will trigger error in handle_task_command

    # Check for mode in multiple ways:
    # 1. Positional argument at start (fg, bg, fix, up)
    # 2. --bg flag anywhere (for backward compatibility)
    if args_copy and args_copy[0] in ["fg", "bg", "fix", "up"]:
        mode_str = args_copy[0]
        if mode_str == "fg":
            mode = AgentMode.FOREGROUND
        elif mode_str == "bg":
            mode = AgentMode.BACKGROUND
        elif mode_str == "fix":
            mode = AgentMode.FIX
        elif mode_str == "up":
            mode = AgentMode.UP
        args_copy = args_copy[1:]  # Remove the positional arg
    elif "--bg" in args_copy:
        mode = AgentMode.BACKGROUND
        # Keep --bg flag for backward compatibility

    return RouterArgs(
        mode=mode,
        remaining_args=args_copy,
        login=login,
        task=task,
        lint=lint,
        test=test,
        fix=fix,
        fix_url=fix_url,
        codeup=codeup,
        codeup_publish=codeup_publish,
        kanban=kanban,
        help=help_requested,
    )
