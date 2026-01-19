#!/usr/bin/env python3
"""Unified argument parsing for clud CLI and agent execution."""

import argparse
import sys
from dataclasses import dataclass
from enum import Enum


class AgentMode(Enum):
    """Agent execution modes."""

    FIX = "fix"
    UP = "up"
    DEFAULT = "default"


@dataclass
class Args:
    """Unified arguments for CLI routing and agent execution."""

    # Router-level arguments (special commands)
    mode: AgentMode
    task: str | None = None
    lint: bool = False
    test: bool = False
    fix: bool = False
    fix_url: str | None = None
    up_publish: bool = False  # For 'clud up -p' or 'clud up --publish'
    init_loop: bool = False
    install_claude: bool = False
    info: bool = False
    help: bool = False
    hook_debug: bool = False  # For --hook-debug (verbose hook logging)
    cron: bool = False  # For --cron (cron scheduler)
    cron_subcommand: str | None = None  # Cron subcommand (add, list, remove, etc.)
    cron_args: list[str] = None  # type: ignore  # Arguments for cron subcommand
    daemon: bool = False  # For --daemon (multi-terminal daemon)
    num_terminals: int = 8  # Number of terminals for --daemon (default 8)
    # Agent-level arguments (execution)
    prompt: str | None = None
    message: str | None = None
    cmd: str | None = None
    continue_flag: bool = False
    dry_run: bool = False
    verbose: bool = False
    idle_timeout: float | None = None
    loop_count: int | None = None
    loop_value: str | None = None  # Raw value from --loop for flexible parsing
    loop_count_override: int | None = None  # Explicit override via --loop-count
    plain: bool = False  # For --plain (disable JSON formatting, enable raw text I/O)
    claude_args: list[str] | None = None


def parse_args(args: list[str] | None = None) -> Args:
    """Parse CLI arguments to determine routing mode and agent configuration."""
    if args is None:
        args = sys.argv[1:]

    # Create a copy to modify
    args_copy = args[:]
    mode = AgentMode.DEFAULT  # Default mode

    # Check for special commands first (these don't need agent routing)
    lint = "--lint" in args_copy
    test = "--test" in args_copy
    fix = "--fix" in args_copy
    init_loop = "--init-loop" in args_copy
    install_claude = "--install-claude" in args_copy
    info = "--info" in args_copy
    hook_debug = "--hook-debug" in args_copy
    cron = "--cron" in args_copy
    daemon = "--daemon" in args_copy or "-d" in args_copy

    # Remove --hook-debug from args_copy since it's handled by router
    if "--hook-debug" in args_copy:
        args_copy.remove("--hook-debug")

    # Remove --daemon or -d from args_copy since it's handled by router
    if "--daemon" in args_copy:
        args_copy.remove("--daemon")
    if "-d" in args_copy:
        args_copy.remove("-d")

    # Extract --num-terminals argument if present (for --daemon)
    num_terminals = 8  # Default value
    if "--num-terminals" in args_copy:
        nt_idx = args_copy.index("--num-terminals")
        args_copy.pop(nt_idx)  # Remove --num-terminals flag
        if nt_idx < len(args_copy) and not args_copy[nt_idx].startswith("-"):
            try:
                num_terminals = int(args_copy[nt_idx])
                args_copy.pop(nt_idx)  # Remove the value
            except ValueError:
                pass  # Keep default if value is not a valid integer

    # Extract cron subcommand and arguments if present
    cron_subcommand = None
    cron_args: list[str] = []
    if "--cron" in args_copy:
        cron_idx = args_copy.index("--cron")
        args_copy.pop(cron_idx)  # Remove --cron flag
        # Extract all remaining args as cron subcommand and args
        if cron_idx < len(args_copy):
            cron_subcommand = args_copy[cron_idx]
            args_copy.pop(cron_idx)  # Remove subcommand
            # Remaining args are cron arguments
            while cron_idx < len(args_copy) and not args_copy[cron_idx].startswith("--"):
                cron_args.append(args_copy[cron_idx])
                args_copy.pop(cron_idx)

    # Extract fix URL argument if present
    fix_url = None
    if "--fix" in args_copy:
        fix_idx = args_copy.index("--fix")
        # Check if there's a URL argument after --fix
        if fix_idx + 1 < len(args_copy) and not args_copy[fix_idx + 1].startswith("-"):
            fix_url = args_copy[fix_idx + 1]

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

    # Parse agent-level arguments using argparse
    parser = argparse.ArgumentParser(
        prog="clud",
        description="Claude Code in YOLO mode - runs Claude with --dangerously-skip-permissions",
        epilog="All unknown arguments are passed directly to Claude Code. WARNING: This mode removes all safety guardrails. Use with caution.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        add_help=False,  # We handle help ourselves
    )

    parser.add_argument(
        "-p",
        "--prompt",
        type=str,
        help="Run Claude with this prompt and exit when complete",
    )

    parser.add_argument(
        "-m",
        "--message",
        type=str,
        help="Send this message to Claude (strips the -m flag)",
    )

    parser.add_argument(
        "--cmd",
        type=str,
        help="Command to execute directly without interactive mode",
    )

    parser.add_argument(
        "-c",
        "--continue",
        action="store_true",
        dest="continue_flag",
        help="Continue previous conversation (adds --continue flag to Claude)",
    )

    parser.add_argument(
        "--dry-run",
        action="store_true",
        dest="dry_run",
        help="Print what would be executed without actually running Claude",
    )

    parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        dest="verbose",
        help="Show debug output",
    )

    parser.add_argument(
        "--idle-timeout",
        type=float,
        dest="idle_timeout",
        help="Timeout in seconds for agent completion detection (enables auto-quit on idle)",
    )

    parser.add_argument(
        "--loop",
        type=str,
        nargs="?",
        const="",  # Empty string when --loop is used without value
        dest="loop_value",
        help=(
            "Run loop mode with a message or file path. Usage: --loop 'msg', --loop LOOP.md (expands to template), or --loop (prompts for message). Use --loop-count to specify iterations. Uses -p."
        ),
    )

    parser.add_argument(
        "--loop-count",
        type=int,
        dest="loop_count_override",
        help="Override the default loop iteration count (default: 50)",
    )

    parser.add_argument(
        "--plain",
        action="store_true",
        dest="plain",
        help="Disable JSON formatting and use raw text I/O",
    )

    # Parse known args, allowing unknown args to be passed to Claude
    known_args, unknown_args = parser.parse_known_args(args_copy)

    return Args(
        # Router-level
        mode=mode,
        task=task,
        lint=lint,
        test=test,
        fix=fix,
        fix_url=fix_url,
        up_publish=up_publish,
        init_loop=init_loop,
        install_claude=install_claude,
        info=info,
        help=help_requested,
        hook_debug=hook_debug,
        cron=cron,
        cron_subcommand=cron_subcommand,
        cron_args=cron_args,
        daemon=daemon,
        num_terminals=num_terminals,
        # Agent-level
        prompt=known_args.prompt,
        message=known_args.message,
        cmd=known_args.cmd,
        continue_flag=known_args.continue_flag,
        dry_run=known_args.dry_run,
        verbose=known_args.verbose,
        idle_timeout=known_args.idle_timeout,
        loop_count=None,  # Will be parsed from loop_value in agent.py
        loop_value=known_args.loop_value,
        loop_count_override=known_args.loop_count_override,
        plain=known_args.plain,
        claude_args=unknown_args,
    )
