#!/usr/bin/env python3
"""Argument parsing for the foreground agent."""

import argparse
from dataclasses import dataclass


@dataclass
class Args:
    """Typed arguments for the yolo command."""

    prompt: str | None
    message: str | None
    cmd: str | None
    continue_flag: bool
    dry_run: bool
    verbose: bool
    claude_args: list[str]
    # Messaging/notification arguments
    notify_user: str | None = None
    notify_interval: int = 30


def parse_args(args: list[str] | None = None) -> Args:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        prog="yolo",
        description="Launch Claude Code with dangerous mode (--dangerously-skip-permissions). This bypasses all permission prompts for a more streamlined workflow.",
        epilog="All unknown arguments are passed directly to Claude Code. WARNING: This mode removes all safety guardrails. Use with caution.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
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
        "--notify-user",
        type=str,
        dest="notify_user",
        help="Send status updates via Telegram/SMS/WhatsApp (format: +1234567890, @username, telegram:123456789, whatsapp:+1234567890)",
    )

    parser.add_argument(
        "--notify-interval",
        type=int,
        default=30,
        dest="notify_interval",
        help="Seconds between progress updates (default: 30)",
    )

    # Parse known args, allowing unknown args to be passed to Claude
    known_args, unknown_args = parser.parse_known_args(args)

    return Args(
        prompt=known_args.prompt,
        message=known_args.message,
        cmd=known_args.cmd,
        continue_flag=known_args.continue_flag,
        dry_run=known_args.dry_run,
        verbose=known_args.verbose,
        claude_args=unknown_args,
        notify_user=known_args.notify_user,
        notify_interval=known_args.notify_interval,
    )
