#!/usr/bin/env python3
"""Unified argument parsing for clud CLI and agent execution."""

import argparse
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
class Args:
    """Unified arguments for CLI routing and agent execution."""

    # Router-level arguments (special commands)
    mode: AgentMode
    login: bool = False
    task: str | None = None
    lint: bool = False
    test: bool = False
    fix: bool = False
    fix_url: str | None = None
    up_publish: bool = False  # For 'clud up -p' or 'clud up --publish'
    kanban: bool = False
    telegram_web: bool = False  # For --telegram/-tg (web app mode)
    telegram_token: str | None = None  # Token for telegram web app
    telegram_server: bool = False  # For --telegram-server (advanced integration)
    telegram_server_port: int | None = None  # Port for telegram server
    telegram_server_config: str | None = None  # Path to telegram config file
    code: bool = False
    code_port: int | None = None
    webui: bool = False
    webui_port: int | None = None
    api_server: bool = False  # For --api-server (REST API mode)
    api_port: int | None = None  # Port for API server
    init_loop: bool = False
    help: bool = False
    track: bool = False
    hook_debug: bool = False  # For --hook-debug (verbose hook logging)
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
    plain: bool = False  # For --plain (disable JSON formatting, enable raw text I/O)
    telegram: bool = False  # For --telegram (notification mode)
    telegram_bot_token: str | None = None
    telegram_chat_id: str | None = None
    claude_args: list[str] | None = None


def parse_args(args: list[str] | None = None) -> Args:
    """Parse CLI arguments to determine routing mode and agent configuration."""
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
    telegram_web = "--telegram" in args_copy or "-tg" in args_copy
    telegram_server = "--telegram-server" in args_copy
    code = "--code" in args_copy
    webui = "--webui" in args_copy
    api_server = "--api-server" in args_copy
    init_loop = "--init-loop" in args_copy
    track = "--track" in args_copy
    hook_debug = "--hook-debug" in args_copy

    # Remove --track from args_copy since it's handled by router
    if "--track" in args_copy:
        args_copy.remove("--track")

    # Remove --hook-debug from args_copy since it's handled by router
    if "--hook-debug" in args_copy:
        args_copy.remove("--hook-debug")

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

    # Extract api port argument if present
    api_port = None
    if "--api-server" in args_copy:
        api_idx = args_copy.index("--api-server")
        # Check if there's a port argument after --api-server
        if api_idx + 1 < len(args_copy) and not args_copy[api_idx + 1].startswith("-"):
            with contextlib.suppress(ValueError):
                api_port = int(args_copy[api_idx + 1])

    # Extract telegram server port and config arguments if present
    telegram_server_port = None
    telegram_server_config = None
    if "--telegram-server" in args_copy:
        tg_server_idx = args_copy.index("--telegram-server")
        # Check for optional port argument
        if tg_server_idx + 1 < len(args_copy) and not args_copy[tg_server_idx + 1].startswith("-"):
            with contextlib.suppress(ValueError):
                telegram_server_port = int(args_copy[tg_server_idx + 1])
        # Check for --telegram-config flag
        if "--telegram-config" in args_copy:
            tg_config_idx = args_copy.index("--telegram-config")
            if tg_config_idx + 1 < len(args_copy) and not args_copy[tg_config_idx + 1].startswith("-"):
                telegram_server_config = args_copy[tg_config_idx + 1]

    # Extract telegram token argument if present
    telegram_token = None
    if "--telegram" in args_copy:
        telegram_idx = args_copy.index("--telegram")
        # Check if there's a token argument after --telegram
        if telegram_idx + 1 < len(args_copy) and not args_copy[telegram_idx + 1].startswith("-"):
            telegram_token = args_copy[telegram_idx + 1]
            # Remove both the flag and its value
            args_copy.pop(telegram_idx)  # Remove flag
            args_copy.pop(telegram_idx)  # Remove value (now at same index)
        else:
            # No token argument, just remove the flag
            args_copy.pop(telegram_idx)
    elif "-tg" in args_copy:
        tg_idx = args_copy.index("-tg")
        # Check if there's a token argument after -tg
        if tg_idx + 1 < len(args_copy) and not args_copy[tg_idx + 1].startswith("-"):
            telegram_token = args_copy[tg_idx + 1]
            # Remove both the flag and its value
            args_copy.pop(tg_idx)  # Remove flag
            args_copy.pop(tg_idx)  # Remove value (now at same index)
        else:
            # No token argument, just remove the flag
            args_copy.pop(tg_idx)

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
        help="Run N times, checking for DONE.md after each. Usage: --loop 50 -p 'msg', --loop 'msg' (prompts count), --loop 50 (prompts msg), or --loop (prompts both). Uses -p.",
    )

    # Telegram notifications (different from --telegram web app)
    parser.add_argument(
        "--telegram-notify",
        action="store_true",
        dest="telegram_notify",
        help="Enable Telegram notifications during agent execution",
    )

    parser.add_argument(
        "--telegram-bot-token",
        type=str,
        help="Telegram bot token (or use TELEGRAM_BOT_TOKEN env var)",
    )

    parser.add_argument(
        "--telegram-chat-id",
        type=str,
        help="Telegram chat ID to send messages to (or use TELEGRAM_CHAT_ID env var)",
    )

    parser.add_argument(
        "--plain",
        action="store_true",
        dest="plain",
        help="Disable JSON formatting and use raw text I/O (for web/telegram integration)",
    )

    # Parse known args, allowing unknown args to be passed to Claude
    known_args, unknown_args = parser.parse_known_args(args_copy)

    # Get Telegram credentials with fallback priority:
    # 1. Command-line args
    # 2. Environment variables
    # 3. Saved config file (loaded lazily in agent.py to avoid circular import)
    import os

    telegram_bot_token = known_args.telegram_bot_token or os.environ.get("TELEGRAM_BOT_TOKEN")
    telegram_chat_id = known_args.telegram_chat_id or os.environ.get("TELEGRAM_CHAT_ID")

    # Note: Saved credentials will be loaded in agent.py if needed
    # (avoiding circular import by not importing load_telegram_credentials here)

    telegram_enabled = known_args.telegram_notify or bool(telegram_bot_token) or bool(telegram_chat_id)

    return Args(
        # Router-level
        mode=mode,
        login=login,
        task=task,
        lint=lint,
        test=test,
        fix=fix,
        fix_url=fix_url,
        up_publish=up_publish,
        kanban=kanban,
        telegram_web=telegram_web,
        telegram_token=telegram_token,
        telegram_server=telegram_server,
        telegram_server_port=telegram_server_port,
        telegram_server_config=telegram_server_config,
        code=code,
        code_port=code_port,
        webui=webui,
        webui_port=webui_port,
        api_server=api_server,
        api_port=api_port,
        init_loop=init_loop,
        help=help_requested,
        track=track,
        hook_debug=hook_debug,
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
        plain=known_args.plain,
        telegram=telegram_enabled,
        telegram_bot_token=telegram_bot_token,
        telegram_chat_id=telegram_chat_id,
        claude_args=unknown_args,
    )
