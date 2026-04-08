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
    PLAN = "plan"
    DEFAULT = "default"


VALID_BACKENDS = {"claude", "codex"}


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
    up_message: str | None = None  # For 'clud up -m "message"'
    plan_prompt: str | None = None  # For 'clud plan "prompt text"'
    init_loop: bool = False
    install_claude: bool = False
    info: bool = False
    help: bool = False
    hook_debug: bool = False  # For --hook-debug (verbose hook logging)
    no_stop_hook: bool = False  # For --no-stop-hook (disable AGENT_STOP hook)
    cron: bool = False  # For --cron (cron scheduler)
    cron_subcommand: str | None = None  # Cron subcommand (add, list, remove, etc.)
    cron_args: list[str] = None  # type: ignore  # Arguments for cron subcommand
    ui: bool = False  # For --ui (multi-terminal UI with Playwright browser)
    tui: bool = False  # For --tui (Textual TUI for loop mode)
    rebase: bool = False  # For --rebase (auto-rebase to origin HEAD)
    no_skills: bool = False  # For --no-skills (skip auto-install of bundled skills)
    num_terminals: int = 4  # Number of terminals for --ui (default 4)
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
    agent_backend: str | None = None
    session_model: str | None = None
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
    init_loop = "--init-loop" in args_copy
    install_claude = "--install-claude" in args_copy
    info = "--info" in args_copy
    hook_debug = "--hook-debug" in args_copy
    no_stop_hook = "--no-stop-hook" in args_copy
    no_skills = "--no-skills" in args_copy
    has_codex_flag = "--codex" in args_copy
    has_claude_flag = "--claude" in args_copy
    if has_codex_flag and has_claude_flag:
        raise ValueError("Cannot specify both --codex and --claude")
    agent_backend = "codex" if has_codex_flag else "claude" if has_claude_flag else None
    cron = "--cron" in args_copy
    ui = "--ui" in args_copy or "-d" in args_copy
    tui = "--tui" in args_copy
    rebase = False

    # Remove --hook-debug and --no-stop-hook from args_copy since they're handled by router
    if "--hook-debug" in args_copy:
        args_copy.remove("--hook-debug")
    if "--no-stop-hook" in args_copy:
        args_copy.remove("--no-stop-hook")
    if "--no-skills" in args_copy:
        args_copy.remove("--no-skills")
    if has_codex_flag:
        args_copy.remove("--codex")
    if has_claude_flag:
        args_copy.remove("--claude")

    session_model = None
    if "--session-model" in args_copy:
        sm_idx = args_copy.index("--session-model")
        if sm_idx + 1 < len(args_copy):
            session_model = args_copy[sm_idx + 1]
            args_copy.pop(sm_idx)
            args_copy.pop(sm_idx)
        else:
            args_copy.pop(sm_idx)
            session_model = ""
    else:
        for i, arg in enumerate(args_copy):
            if arg.startswith("--session-model="):
                session_model = arg.split("=", 1)[1]
                args_copy.pop(i)
                break

    if session_model is not None and session_model not in VALID_BACKENDS:
        raise ValueError(f"Invalid --session-model value: {session_model}. Expected one of: claude, codex")

    # Remove --ui or -d from args_copy since it's handled by router
    if "--ui" in args_copy:
        args_copy.remove("--ui")
    if "-d" in args_copy:
        args_copy.remove("-d")
    if "--tui" in args_copy:
        args_copy.remove("--tui")

    # Default number of terminals for --ui (4 terminals)
    num_terminals = 4

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

    # Extract fix URL argument if present (from 'clud fix <URL>' subcommand)
    fix_url = None

    # Only intercept help if no mode is specified
    help_requested = ("--help" in args_copy or "-h" in args_copy) and not (args_copy and args_copy[0] in ["fix", "up", "loop", "rebase", "plan"])

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

    # Check for mode: positional subcommands
    up_publish = False
    up_message = None
    loop_value: str | None = None
    loop_count_override: int | None = None
    plan_prompt = None
    if args_copy and args_copy[0] in ["fix", "up", "loop", "rebase", "plan"]:
        mode_str = args_copy[0]
        if mode_str == "fix":
            mode = AgentMode.FIX
            # Check if there's a URL argument after 'fix'
            remaining = args_copy[1:]
            if remaining and not remaining[0].startswith("-"):
                fix_url = remaining[0]
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
            # Check for -m or --message flag after 'up'
            if "-m" in args_copy or "--message" in args_copy:
                message_flag = "-m" if "-m" in args_copy else "--message"
                message_idx = args_copy.index(message_flag)
                if message_idx + 1 < len(args_copy):
                    up_message = args_copy[message_idx + 1]
                    # Remove both the flag and its value from args_copy
                    args_copy.pop(message_idx)  # Remove flag
                    args_copy.pop(message_idx)  # Remove value (now at same index)
        elif mode_str == "rebase":
            rebase = True
        elif mode_str == "plan":
            mode = AgentMode.PLAN
            # Collect remaining args as the plan prompt text
            remaining = args_copy[1:]
            if remaining and not remaining[0].startswith("-"):
                plan_prompt = remaining[0]
                args_copy = args_copy[2:]  # Remove 'plan' and prompt
            else:
                plan_prompt = None  # Will error in handler
                args_copy = args_copy[1:]  # Remove 'plan'
        elif mode_str == "loop":
            # Extract optional loop value (message or file path) after 'loop'
            args_copy = args_copy[1:]  # Remove 'loop'
            # Check for --loop-count flag
            if "--loop-count" in args_copy:
                lc_idx = args_copy.index("--loop-count")
                if lc_idx + 1 < len(args_copy):
                    loop_count_override = int(args_copy[lc_idx + 1])
                    args_copy.pop(lc_idx)  # Remove flag
                    args_copy.pop(lc_idx)  # Remove value
            # Next non-flag arg is the loop value
            if args_copy and not args_copy[0].startswith("-"):
                loop_value = args_copy[0]
                args_copy.pop(0)
            else:
                loop_value = ""  # No value: prompt for message
        if mode_str not in ("loop", "plan"):
            args_copy = args_copy[1:]  # Remove the positional arg (already done for loop/plan)

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
        fix=False,
        fix_url=fix_url,
        up_publish=up_publish,
        up_message=up_message,
        plan_prompt=plan_prompt,
        init_loop=init_loop,
        install_claude=install_claude,
        info=info,
        help=help_requested,
        hook_debug=hook_debug,
        no_stop_hook=no_stop_hook,
        no_skills=no_skills,
        cron=cron,
        cron_subcommand=cron_subcommand,
        cron_args=cron_args,
        ui=ui,
        tui=tui,
        rebase=rebase,
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
        loop_value=loop_value,
        loop_count_override=loop_count_override,
        plain=known_args.plain,
        agent_backend=agent_backend,
        session_model=session_model,
        claude_args=unknown_args,
    )
