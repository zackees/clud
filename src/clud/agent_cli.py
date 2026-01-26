"""Consolidated agent module - handles all Claude Code execution and special commands."""

import logging
import sys

# Import command handlers
from .agent.commands import (
    handle_codeup_command,
    handle_codeup_publish_command,
    handle_fix_command,
    handle_info_command,
    handle_init_loop_command,
    handle_install_claude_command,
    handle_lint_command,
    handle_test_command,
)

# Import from agent modules
from .agent.exceptions import ConfigError, ValidationError
from .agent.runner import run_agent
from .agent_args import AgentMode, parse_args
from .cron.cli_handler import handle_cron_command
from .daemon.cli_handler import handle_daemon_command
from .task import handle_task_command

# Initialize logger
logger = logging.getLogger(__name__)


def main(args_list: list[str] | None = None) -> int:
    """Main entry point for clud - handles routing and execution."""
    try:
        # Set terminal title early
        # DISABLED: Causes escape sequence artifacts on some terminals (git-bash/mintty)
        # set_terminal_title()

        # Parse arguments
        args = parse_args(args_list)

        # Handle help first
        if args.help:
            print("clud - Claude Code in YOLO mode")
            print("Usage: clud [options...]")
            print()
            print("Pipe mode:")
            print("  echo 'prompt' | clud       Read prompt from stdin (non-TTY mode)")
            print("  clud -p 'prompt' | cat     Pipe output to another command")
            print("  cat file | clud | less     Chain pipes for input and output")
            print()
            print("Special modes:")
            print("  fix [URL]                      Fix linting and test issues (with optional GitHub URL)")
            print("  up [-m MSG] [-p|--publish]     Run global codeup command with auto-fix")
            print()
            print("Special commands:")
            print("  --task PATH          Open task file in editor")
            print("  --lint               Run lint and tests with lint-test")
            print("  --test               Run lint and tests with lint-test")
            print("  --fix [URL]          Fix linting issues and run tests (optionally from GitHub URL)")
            print("  --init-loop          Create LOOP.md index from existing markdown files")
            print("  --cron <subcommand>  Schedule recurring tasks (use 'clud --cron help' for details)")
            print("  --ui, -d             Launch multi-terminal UI with Playwright browser (4 terminals)")
            print("  --info               Show Claude Code installation information")
            print("  --install-claude     Install Claude Code to ~/.clud/npm (self-contained)")
            print("  -h, --help           Show this help")
            print()
            print("Default: Run Claude Code with --dangerously-skip-permissions")
            return 0

        # Handle special commands that don't require agents
        if args.task is not None:
            return handle_task_command(args.task)

        if args.lint:
            return handle_lint_command()

        if args.test:
            return handle_test_command()

        if args.fix:
            return handle_fix_command(args.fix_url)

        if args.init_loop:
            return handle_init_loop_command()

        if args.info:
            return handle_info_command()

        if args.install_claude:
            return handle_install_claude_command()

        if args.cron:
            return handle_cron_command(args.cron_subcommand, args.cron_args)

        if args.ui:
            return handle_daemon_command(args.num_terminals)

        # Route to appropriate mode handler
        if args.mode == AgentMode.FIX:
            return handle_fix_command(args.fix_url)
        elif args.mode == AgentMode.UP:
            # Check if publish flag was provided
            if args.up_publish:
                return handle_codeup_publish_command(args.up_message)
            else:
                return handle_codeup_command(args.up_message)
        else:
            # Default mode - run foreground agent
            return run_agent(args)

    except (ValidationError, ConfigError) as e:
        print(f"Error: {e}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("\nOperation cancelled.", file=sys.stderr)
        return 2
    except Exception as e:
        # Handle other common exceptions that might come from agents
        error_msg = str(e)
        if "Config" in error_msg or "config" in error_msg:
            print(f"Configuration error: {e}", file=sys.stderr)
            return 4
        else:
            print(f"Unexpected error: {e}", file=sys.stderr)
            return 1


if __name__ == "__main__":
    sys.exit(main())
