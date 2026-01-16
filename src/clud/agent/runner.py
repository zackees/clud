"""Core agent execution logic for Claude Code."""

import logging
import os
import subprocess
import sys
import traceback
import uuid
from pathlib import Path
from typing import TYPE_CHECKING

from running_process import RunningProcess

from ..claude_installer import prompt_install_claude

if TYPE_CHECKING:
    from ..agent_args import Args
from ..hooks import HookContext, HookEvent
from ..json_formatter import StreamJsonFormatter, create_formatter_callback
from ..output_filter import OutputFilter
from ..telegram_bot import TelegramBot
from .claude_finder import _find_claude_path
from .command_builder import (
    _build_claude_command,
    _get_model_from_args,
    _print_debug_info,
    _print_error_diagnostics,
    _print_launch_banner,
    _print_model_message,
    _wrap_command_for_git_bash,
)
from .completion import detect_agent_completion
from .config import load_telegram_credentials
from .hooks import register_hooks_from_config, trigger_hook_sync
from .loop_executor import _run_loop
from .subprocess import _execute_command
from .user_input import _prompt_for_message

# Initialize logger
logger = logging.getLogger(__name__)


def run_agent(args: "Args") -> int:
    """
    Launch Claude Code with dangerous mode (--dangerously-skip-permissions).
    This bypasses all permission prompts for a more streamlined workflow.

    WARNING: This mode removes all safety guardrails. Use with caution.
    """
    # Initialize variables for exception handler access
    claude_path: str | None = None
    cmd: list[str] = []

    # Generate unique instance ID for this agent run
    instance_id = str(uuid.uuid4())
    session_id = instance_id  # In standalone mode, session_id equals instance_id

    # Check for piped stdin input (non-TTY mode)
    # This enables: echo "prompt" | clud
    if not sys.stdin.isatty() and not args.prompt and not args.message:
        try:
            # Read all input from stdin
            stdin_input = sys.stdin.read().strip()
            if stdin_input:
                # Use the piped input as the prompt
                args.prompt = stdin_input
        except Exception as e:
            logger.warning(f"Failed to read from stdin: {e}")

    # Register hooks early (before any execution)
    register_hooks_from_config(hook_debug=args.hook_debug)

    # Load telegram credentials from saved config if not already provided
    if args.telegram and (not args.telegram_bot_token or not args.telegram_chat_id):
        saved_token, saved_chat_id = load_telegram_credentials()
        if not args.telegram_bot_token:
            args.telegram_bot_token = saved_token
        if not args.telegram_chat_id:
            args.telegram_chat_id = saved_chat_id

    # Initialize Telegram bot if enabled
    telegram_bot = TelegramBot.from_args(args)

    # Send invitation if telegram bot is available
    if telegram_bot:
        telegram_bot.send_invitation(project_path=Path.cwd(), mode="foreground")

    try:
        # Handle dry-run mode early (before API key check)
        # Dry-run mode doesn't need API key since it only prints the command
        if args.dry_run:
            cmd_parts = ["claude", "--dangerously-skip-permissions"]
            if args.continue_flag:
                cmd_parts.append("--continue")
            if args.prompt:
                cmd_parts.extend(["-p", args.prompt])
                # Enable streaming JSON output for -p flag by default (unless --plain is used)
                # Note: stream-json requires --verbose when used with --print/-p
                if not args.plain:
                    cmd_parts.extend(["--output-format", "stream-json", "--verbose"])
            if args.message:
                cmd_parts.append(args.message)
            if args.claude_args:
                cmd_parts.extend(args.claude_args)
            print("Would execute:", " ".join(cmd_parts))
            return 0

        # If --cmd is provided, execute the command directly instead of launching Claude
        if args.cmd:
            result = subprocess.run(args.cmd, shell=True)
            return result.returncode

        # Set max output tokens for Claude
        os.environ["CLAUDE_CODE_MAX_OUTPUT_TOKENS"] = "64000"
        # Disable Claude git author attribution
        os.environ["CLAUDE_GIT_AUTHOR"] = "0"

        # No validation needed - if no input is provided and stdin is a tty,
        # Claude Code will launch in interactive mode

        # Find Claude executable
        claude_path = _find_claude_path()
        if not claude_path:
            # Claude Code not found - offer to install it locally
            print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
            print(file=sys.stderr)

            # Offer automatic installation
            if prompt_install_claude():
                # Installation succeeded, try finding it again
                claude_path = _find_claude_path()
                if not claude_path:
                    print("Error: Installation succeeded but claude executable still not found", file=sys.stderr)
                    return 1
            else:
                # Installation declined or failed
                print(file=sys.stderr)
                print("You can also:", file=sys.stderr)
                print("  - Install globally: npm install -g @anthropic-ai/claude-code@latest", file=sys.stderr)
                print("  - Install later with: clud --install-claude", file=sys.stderr)
                print("  - Download from: https://claude.ai/download", file=sys.stderr)
                return 1

        # Handle loop mode - parse loop_value flexibly
        if args.loop_value is not None:
            loop_count = None
            loop_message = None

            # Try to parse loop_value
            if args.loop_value == "":
                # --loop with no value: prompt for message
                pass
            else:
                # Check if it's a file path
                # File paths (especially .md files) get expanded to a template message
                if args.loop_value.endswith(".md") or Path(args.loop_value).exists():
                    # For loop files, agent will use working copy in .loop/
                    # Original file remains read-only
                    original_filename = Path(args.loop_value).name
                    working_file_path = f".loop/{original_filename}"

                    # Expand to template message for file-based loop mode
                    # Point agent to working copy in .loop/
                    loop_message = (
                        f"Read {working_file_path} and do the next task. "
                        f"You are free to update {working_file_path} with information critical "
                        f"for the next agent and future agents as this task is worked on."
                    )
                else:
                    # Not a file path, treat as regular message
                    loop_message = args.loop_value

            # Prompt for missing values
            # Check if we have a message from loop_value, -m, or -p
            if not args.prompt and not args.message and not loop_message:
                loop_message = _prompt_for_message()

            # Determine final loop count (priority: --loop-count > default)
            loop_count = args.loop_count_override if args.loop_count_override is not None else 50

            # Set the prompt if we got it from loop_value (uses -p instead of -m)
            if loop_message and not args.message and not args.prompt:
                args.prompt = loop_message

            return _run_loop(args, claude_path, loop_count)

        # Build command
        cmd = _build_claude_command(args, claude_path)
        # Wrap command in git-bash on Windows if available
        cmd = _wrap_command_for_git_bash(cmd)

        # Detect and print model message (for display only)
        model_flag = _get_model_from_args(args.claude_args)
        _print_model_message(model_flag)

        # Print launch banner with command and environment
        env_vars = {
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "64000",
            "CLAUDE_GIT_AUTHOR": "0",
        }
        _print_launch_banner(cmd, env_vars=env_vars)

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Trigger AGENT_START hook
        user_message = args.prompt if args.prompt else args.message if args.message else None
        trigger_hook_sync(
            HookEvent.AGENT_START,
            HookContext(
                event=HookEvent.AGENT_START,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                message=user_message,
            ),
            hook_debug=args.hook_debug,
        )

        # Execute Claude with the dangerous permissions flag
        # Use idle detection if timeout is specified
        returncode = 0  # Initialize returncode for hook triggers

        if args.idle_timeout is not None:
            # Create output filter to suppress terminal capability responses
            output_filter = OutputFilter()

            # Output callback to print data to stdout (with filtering)
            def output_callback(data: str) -> None:
                # Filter out terminal capability responses to prevent corrupting parent terminal
                filtered_data = output_filter.filter_terminal_responses(data)
                if filtered_data:
                    sys.stdout.write(filtered_data)
                    sys.stdout.flush()

            detect_agent_completion(cmd, args.idle_timeout, output_callback)
            returncode = 0
        elif args.prompt:
            # Use RunningProcess for streaming output when using -p flag
            # This ensures stream-json output is displayed line-by-line in real-time
            if args.plain:
                # Plain mode: no JSON formatting, just pass through output
                returncode = RunningProcess.run_streaming(cmd)
            else:
                # Create JSON formatter for beautiful output
                formatter = StreamJsonFormatter(
                    show_system=args.verbose,
                    show_usage=True,
                    show_cache=args.verbose,
                    verbose=args.verbose,
                )
                stdout_callback = create_formatter_callback(formatter)
                returncode = RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback)
        else:
            returncode = _execute_command(cmd, use_shell=False, verbose=args.verbose)

        # Trigger POST_EXECUTION hook after successful completion
        trigger_hook_sync(
            HookEvent.POST_EXECUTION,
            HookContext(
                event=HookEvent.POST_EXECUTION,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                message=user_message,
                metadata={"returncode": returncode},
            ),
            hook_debug=args.hook_debug,
        )

        return returncode

    except FileNotFoundError as e:
        error_msg = f"Claude Code is not installed or not in PATH: {e}"
        print(f"Error: {error_msg}", file=sys.stderr)
        print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
        print(f"DEBUG: FileNotFoundError details: {e}", file=sys.stderr)
        traceback.print_exc()

        # Trigger ERROR hook
        trigger_hook_sync(
            HookEvent.ERROR,
            HookContext(
                event=HookEvent.ERROR,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                error=error_msg,
            ),
            hook_debug=args.hook_debug,
        )
        return 1

    except KeyboardInterrupt:
        print("\nInterrupted by user", file=sys.stderr)

        # Trigger AGENT_STOP hook on interrupt
        trigger_hook_sync(
            HookEvent.AGENT_STOP,
            HookContext(
                event=HookEvent.AGENT_STOP,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                metadata={"reason": "interrupted"},
            ),
            hook_debug=args.hook_debug,
        )
        return 130

    except OSError as e:
        error_msg = f"OS error launching Claude: {e}"

        # Try backup method with shell=True first (Windows shell script issue)
        # Only show error if backup also fails
        if cmd and claude_path:
            try:
                # Silently try shell=True method first
                if args.verbose:
                    print(f"DEBUG: OSError {e.winerror if hasattr(e, 'winerror') else e.errno}, retrying with shell=True...", file=sys.stderr)
                return _execute_command(cmd, use_shell=True, verbose=args.verbose)
            except Exception as shell_error:
                # Both methods failed - now show full error details
                print(f"Error launching Claude: {e}", file=sys.stderr)
                print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
                _print_error_diagnostics(claude_path, cmd)
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()
                # Fall through to trigger ERROR hook and return 1
        else:
            # Can't attempt backup
            print(f"Error launching Claude: {e}", file=sys.stderr)
            print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
            _print_error_diagnostics(claude_path, cmd)
            print("\nFull stack trace from original error:", file=sys.stderr)
            traceback.print_exc()
            # Fall through to trigger ERROR hook and return 1

        # Trigger ERROR hook (reached when both methods fail or backup can't be attempted)
        trigger_hook_sync(
            HookEvent.ERROR,
            HookContext(
                event=HookEvent.ERROR,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                error=error_msg,
            ),
            hook_debug=args.hook_debug,
        )
        return 1

    except Exception as e:
        error_msg = f"Unexpected error launching Claude: {e}"
        print(f"Error launching Claude: {e}", file=sys.stderr)
        print(f"DEBUG: Exception type: {type(e).__name__}", file=sys.stderr)
        _print_error_diagnostics(claude_path, cmd)

        # Try backup method with shell=True
        if cmd and claude_path:
            try:
                print("\nAttempting backup method (shell=True)...", file=sys.stderr)
                return _execute_command(cmd, use_shell=True, verbose=args.verbose)
            except Exception as shell_error:
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()

        print("\nFull stack trace from original error:", file=sys.stderr)
        traceback.print_exc()

        # Trigger ERROR hook
        trigger_hook_sync(
            HookEvent.ERROR,
            HookContext(
                event=HookEvent.ERROR,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                error=error_msg,
            ),
            hook_debug=args.hook_debug,
        )
        return 1

    finally:
        # Send cleanup notification if telegram bot is available
        if telegram_bot:
            telegram_bot.send_cleanup()

        # Trigger AGENT_STOP hook in finally block
        trigger_hook_sync(
            HookEvent.AGENT_STOP,
            HookContext(
                event=HookEvent.AGENT_STOP,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
            ),
            hook_debug=args.hook_debug,
        )
