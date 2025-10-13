#!/usr/bin/env python3
"""Simplified CLI routing for clud."""

import contextlib
import sys
from dataclasses import dataclass
from enum import Enum


class AgentMode(Enum):
    """Agent execution modes."""

    FIX = "fix"
    UP = "up"
    DEFAULT = "default"


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
    up_publish: bool = False  # For 'clud up -p' or 'clud up --publish'
    kanban: bool = False
    telegram: bool = False
    telegram_token: str | None = None
    code: bool = False
    code_port: int | None = None
    webui: bool = False
    webui_port: int | None = None
    init_loop: bool = False
    help: bool = False
    track: bool = False


def parse_router_args(args: list[str] | None = None) -> RouterArgs:
    """Parse CLI arguments to determine routing mode and extract special commands."""
    if args is None:
        args = sys.argv[1:]

    # Create a copy to modify
    args_copy = args[:]
    mode = AgentMode.DEFAULT  # Default mode

    # Check for special commands first (these don't need agent routing)
    login = "--login" in args_copy
    lint = "--lint" in args_copy
    test = "--test" in args_copy
    fix = "--fix" in args_copy
    kanban = "--kanban" in args_copy
    telegram = "--telegram" in args_copy or "-tg" in args_copy
    code = "--code" in args_copy
    webui = "--webui" in args_copy
    init_loop = "--init-loop" in args_copy
    track = "--track" in args_copy

    # Remove --track from args_copy since it's handled by router
    if "--track" in args_copy:
        args_copy.remove("--track")

    # Extract fix URL argument if present
    fix_url = None
    if "--fix" in args_copy:
        fix_idx = args_copy.index("--fix")
        # Check if there's a URL argument after --fix
        if fix_idx + 1 < len(args_copy) and not args_copy[fix_idx + 1].startswith("-"):
            fix_url = args_copy[fix_idx + 1]

    # Extract code port argument if present
    code_port = None
    if "--code" in args_copy:
        code_idx = args_copy.index("--code")
        # Check if there's a port argument after --code
        if code_idx + 1 < len(args_copy) and not args_copy[code_idx + 1].startswith("-"):
            with contextlib.suppress(ValueError):
                code_port = int(args_copy[code_idx + 1])

    # Extract webui port argument if present
    webui_port = None
    if "--webui" in args_copy:
        webui_idx = args_copy.index("--webui")
        # Check if there's a port argument after --webui
        if webui_idx + 1 < len(args_copy) and not args_copy[webui_idx + 1].startswith("-"):
            with contextlib.suppress(ValueError):
                webui_port = int(args_copy[webui_idx + 1])

    # Extract telegram token argument if present
    telegram_token = None
    if "--telegram" in args_copy:
        telegram_idx = args_copy.index("--telegram")
        # Check if there's a token argument after --telegram
        if telegram_idx + 1 < len(args_copy) and not args_copy[telegram_idx + 1].startswith("-"):
            telegram_token = args_copy[telegram_idx + 1]
    elif "-tg" in args_copy:
        tg_idx = args_copy.index("-tg")
        # Check if there's a token argument after -tg
        if tg_idx + 1 < len(args_copy) and not args_copy[tg_idx + 1].startswith("-"):
            telegram_token = args_copy[tg_idx + 1]

    # Only intercept help if no mode is specified
    help_requested = ("--help" in args_copy or "-h" in args_copy) and not (args_copy and args_copy[0] in ["fix", "up"])

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

    # Check for mode: only fix and up are special modes now
    up_publish = False
    if args_copy and args_copy[0] in ["fix", "up"]:
        mode_str = args_copy[0]
        if mode_str == "fix":
            mode = AgentMode.FIX
        elif mode_str == "up":
            mode = AgentMode.UP
            # Check for -p or --publish flag after 'up'
            if "-p" in args_copy or "--publish" in args_copy:
                up_publish = True
                # Remove the publish flag from args
                if "-p" in args_copy:
                    args_copy.remove("-p")
                if "--publish" in args_copy:
                    args_copy.remove("--publish")
        args_copy = args_copy[1:]  # Remove the positional arg

    return RouterArgs(
        mode=mode,
        remaining_args=args_copy,
        login=login,
        task=task,
        lint=lint,
        test=test,
        fix=fix,
        fix_url=fix_url,
        up_publish=up_publish,
        kanban=kanban,
        telegram=telegram,
        telegram_token=telegram_token,
        code=code,
        code_port=code_port,
        webui=webui,
        webui_port=webui_port,
        init_loop=init_loop,
        help=help_requested,
        track=track,
    )
