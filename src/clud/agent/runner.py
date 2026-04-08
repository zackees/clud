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
from ..skill_installer import install_skills, needs_install
from .arg_translation import to_agent_args
from .backends.registry import get_backend
from .prompts import LOOP_PROMPT_TEMPLATE

if TYPE_CHECKING:
    from ..agent_args import Args
from ..hooks import HookContext, HookEvent
from ..hooks.claude_compat import get_codex_stop_hook_idle_timeout
from ..json_formatter import StreamJsonFormatter, create_formatter_callback
from ..output_filter import OutputFilter
from ..util import handle_keyboard_interrupt
from .claude_finder import _find_claude_path
from .command_builder import (
    _get_effective_backend,
    _persist_backend_selection,
    _print_debug_info,
    _print_error_diagnostics,
    _print_launch_banner,
    _wrap_command_for_git_bash,
)
from .completion import detect_agent_completion
from .hooks import HookRegistrationSummary, register_hooks_from_config, trigger_hook_sync
from .loop_executor import _run_loop
from .process_launcher import run_claude_process
from .subprocess import _execute_command
from .user_input import _prompt_for_message

# Initialize logger
logger = logging.getLogger(__name__)


def _find_backend_executable(backend: str) -> str | None:
    """Find the executable for the selected backend."""
    if backend == "claude":
        return _find_claude_path()
    try:
        return get_backend(backend).find_executable()
    except KeyError:
        return None


def run_agent(args: "Args") -> int:
    """
    Launch Claude Code with dangerous mode (--dangerously-skip-permissions).
    This bypasses all permission prompts for a more streamlined workflow.

    WARNING: This mode removes all safety guardrails. Use with caution.
    """
    # Initialize variables for exception handler access
    claude_path: str | None = None
    backend: str | None = None
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

    # Validate --tui requires --loop
    if args.tui and args.loop_value is None:
        print("Error: --tui requires loop subcommand", file=sys.stderr)
        return 2

    # Register hooks early (before any execution)
    hook_summary = HookRegistrationSummary()
    if not args.no_hooks:
        hook_summary = register_hooks_from_config(hook_debug=args.hook_debug, cwd=Path.cwd())

    try:
        _persist_backend_selection(args)
        backend = _get_effective_backend(args)
        backend_adapter = get_backend(backend)
        agent_args = to_agent_args(args, resolved_backend=backend, cwd=os.getcwd())

        # Handle dry-run mode early (before API key check)
        # Dry-run mode doesn't need API key since it only prints the command
        if args.dry_run:
            # Handle loop mode dry-run
            if args.loop_value is not None:
                loop_count = args.loop_count_override if args.loop_count_override is not None else 50

                # Determine the loop message based on loop_value type
                working_file_path = ".loop/LOOP.md"  # Default
                if args.loop_value == "":
                    loop_prompt = "<would prompt for message>"
                elif args.loop_value.endswith(".md") or Path(args.loop_value).exists():
                    original_filename = Path(args.loop_value).name
                    working_file_path = f".loop/{original_filename}"
                    loop_prompt = LOOP_PROMPT_TEMPLATE.format(working_file_path=working_file_path)
                else:
                    working_file_path = ".loop/LOOP.md"
                    loop_prompt = LOOP_PROMPT_TEMPLATE.format(working_file_path=working_file_path)

                print(f"Loop mode: {loop_count} iterations")
                print(f"Working file: {working_file_path if args.loop_value else '.loop/LOOP.md'}")
                if args.loop_value and not args.loop_value.endswith(".md") and not Path(args.loop_value).exists():
                    print("String prompt will be written to: .loop/LOOP.md")
                    print(f"Original prompt: {args.loop_value}")
                print(f"Loop prompt: {loop_prompt}")
                print()
                if backend == "codex":
                    cmd_parts = [
                        "codex",
                        "--dangerously-bypass-approvals-and-sandbox",
                        "-C",
                        os.getcwd(),
                        "exec",
                        f'"{loop_prompt}"',
                    ]
                else:
                    cmd_parts = ["claude", "--dangerously-skip-permissions", "-p", f'"{loop_prompt}"']
                if backend == "claude" and not args.plain:
                    cmd_parts.extend(["--output-format", "stream-json", "--verbose"])
                if args.claude_args:
                    cmd_parts.extend(args.claude_args)
                print("Would execute:", " ".join(cmd_parts))
                return 0

            # Handle regular (non-loop) dry-run
            plan = backend_adapter.build_launch_plan(agent_args)
            print("Would execute:", " ".join([backend, *plan.argv]))
            return 0

        # If --cmd is provided, execute the command directly instead of launching Claude
        if args.cmd:
            result = subprocess.run(args.cmd, shell=True)
            return result.returncode

        # Set environment variable to indicate we're running inside clud
        os.environ["IN_CLUD"] = "1"
        if backend == "claude":
            os.environ["CLAUDE_CODE_MAX_OUTPUT_TOKENS"] = "64000"
        # Disable MSYS/git-bash automatic path conversion
        # This prevents URLs like https://github.com/... from being converted to
        # Windows paths like https;\\github.com\... when running through git-bash
        os.environ["MSYS_NO_PATHCONV"] = "1"
        os.environ["MSYS2_ARG_CONV_EXCL"] = "*"

        # No validation needed - if no input is provided and stdin is a tty,
        # Claude Code will launch in interactive mode

        # Find backend executable
        claude_path = _find_backend_executable(backend)
        if not claude_path:
            print(f"Error: {backend.title()} is not installed or not in PATH", file=sys.stderr)
            print(file=sys.stderr)

            if backend == "claude":
                if prompt_install_claude():
                    claude_path = _find_backend_executable(backend)
                    if not claude_path:
                        print("Error: Installation succeeded but claude executable still not found", file=sys.stderr)
                        return 1
                else:
                    print(file=sys.stderr)
                    print("You can also:", file=sys.stderr)
                    for line in backend_adapter.install_help():
                        print(f"  - {line}", file=sys.stderr)
                    print("  - Install later with: clud --install-claude", file=sys.stderr)
                    return 1
            else:
                for line in backend_adapter.install_help():
                    print(line, file=sys.stderr)
                return 1

        # Auto-install bundled skills/agents/rules on first run or upgrade
        if not args.no_skills and needs_install():
            install_skills()

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
                    loop_message = LOOP_PROMPT_TEMPLATE.format(working_file_path=working_file_path)
                else:
                    # Not a file path - will write to .loop/LOOP.md in loop_executor
                    # Use same template message as file paths for consistency
                    working_file_path = ".loop/LOOP.md"
                    loop_message = LOOP_PROMPT_TEMPLATE.format(working_file_path=working_file_path)

            # Prompt for missing values
            # Check if we have a message from loop_value, -m, or -p
            if not args.prompt and not args.message and not loop_message:
                loop_message = _prompt_for_message()

            # Determine final loop count (priority: --loop-count > default)
            loop_count = args.loop_count_override if args.loop_count_override is not None else 50

            # Set the prompt if we got it from loop_value (uses -p instead of -m)
            if loop_message and not args.message and not args.prompt:
                args.prompt = loop_message

            if args.tui:
                from ..loop_tui.integration import run_loop_with_tui

                return run_loop_with_tui(args, claude_path, loop_count)

            # Wrap _run_loop with KeyboardInterrupt handler (matches TUI pattern)
            # This ensures Ctrl-C is properly caught and handled at the top level
            try:
                return _run_loop(args, claude_path, loop_count)
            except KeyboardInterrupt as e:
                # Clean exit on Ctrl-C (cleanup already done in _run_loop)
                print("\n⚠️  Loop interrupted by user. Session info saved to .loop/info.json", file=sys.stderr)
                handle_keyboard_interrupt(e)
                return 130  # Worker thread: suppressed

        # Build command
        plan = backend_adapter.build_launch_plan(agent_args)
        plan.executable = claude_path
        cmd = plan.command
        # Claude benefits from git-bash on Windows, but wrapping Codex breaks
        # its native interactive TUI input handling.
        if backend == "claude":
            cmd = _wrap_command_for_git_bash(cmd)

        # Detect and print model message (for display only)
        model_flag = plan.model_display
        if backend == "codex":
            if model_flag:
                print(f"Loading Codex model {model_flag}...", file=sys.stderr)
        elif model_flag == "--haiku" or model_flag == "haiku":
            print("Loading Haiku 4.5...", file=sys.stderr)
        elif model_flag == "--sonnet" or model_flag == "sonnet":
            print("Loading Sonnet 4.5...", file=sys.stderr)
        elif model_flag:
            display_name = model_flag.lstrip("-").replace("-", " ").title()
            print(f"Loading {display_name}...", file=sys.stderr)

        # Print launch banner with command and environment
        env_vars = plan.env
        _print_launch_banner(cmd, env_vars=env_vars, backend=backend)

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Trigger AGENT_START hook
        user_message = args.prompt if args.prompt else args.message if args.message else None
        if not args.no_hooks:
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
        returncode = 0
        idle_detected = False
        stop_reason = "process_exit"
        effective_idle_timeout = args.idle_timeout
        if effective_idle_timeout is None and backend == "codex" and plan.interactive and hook_summary.has_post_execution_hooks:
            effective_idle_timeout = get_codex_stop_hook_idle_timeout()
            print(
                f"Claude-compatible Stop hooks detected; using Codex idle timeout {effective_idle_timeout:.1f}s",
                file=sys.stderr,
            )

        if effective_idle_timeout is not None:
            # Create output filter to suppress terminal capability responses
            output_filter = OutputFilter()

            # Output callback to print data to stdout (with filtering)
            def output_callback(data: str) -> None:
                # Filter out terminal capability responses to prevent corrupting parent terminal
                filtered_data = output_filter.filter_terminal_responses(data)
                if filtered_data:
                    sys.stdout.write(filtered_data)
                    sys.stdout.flush()

            detection = detect_agent_completion(cmd, effective_idle_timeout, output_callback)
            idle_detected = detection.idle_detected
            returncode = detection.returncode
            if idle_detected:
                stop_reason = "idle_detected"
        elif args.prompt:
            # Use RunningProcess for streaming output when using -p flag
            # This ensures stream-json output is displayed line-by-line in real-time
            if args.plain or backend == "codex":
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
            # Use run_claude_process for interactive mode to get proper
            # process group isolation (CREATE_NEW_PROCESS_GROUP on Windows).
            # This prevents Ctrl-C from reaching the child process tree,
            # avoiding ugly tracebacks from nodejs_wheel's Python wrapper.
            returncode = run_claude_process(cmd, propagate_keyboard_interrupt=False)

        # Trigger POST_EXECUTION hook after successful completion
        if not args.no_hooks:
            trigger_hook_sync(
                HookEvent.POST_EXECUTION,
                HookContext(
                    event=HookEvent.POST_EXECUTION,
                    instance_id=instance_id,
                    session_id=session_id,
                    client_type="cli",
                    client_id="standalone",
                    message=user_message,
                    metadata={
                        "backend": backend,
                        "cwd": os.getcwd(),
                        "idle_detected": idle_detected,
                        "reason": stop_reason,
                        "returncode": returncode,
                    },
                ),
                hook_debug=args.hook_debug,
            )

        return returncode

    except FileNotFoundError as e:
        error_msg = f"Agent executable is not installed or not in PATH: {e}"
        print(f"Error: {error_msg}", file=sys.stderr)
        print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
        print(f"DEBUG: FileNotFoundError details: {e}", file=sys.stderr)
        traceback.print_exc()

        # Trigger ERROR hook
        if not args.no_hooks:
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

    except KeyboardInterrupt as e:
        print("\nInterrupted by user", file=sys.stderr)
        handle_keyboard_interrupt(e, reraise_on_main_thread=False)
        stop_reason = "interrupted"
        returncode = 130
        return 130  # Worker thread: suppressed

    except OSError as e:
        error_msg = f"OS error launching agent: {e}"

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
                print(f"Error launching agent: {e}", file=sys.stderr)
                print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
                _print_error_diagnostics(claude_path, cmd)
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()
                # Fall through to trigger ERROR hook and return 1
        else:
            # Can't attempt backup
            print(f"Error launching agent: {e}", file=sys.stderr)
            print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
            _print_error_diagnostics(claude_path, cmd)
            print("\nFull stack trace from original error:", file=sys.stderr)
            traceback.print_exc()
            # Fall through to trigger ERROR hook and return 1

        # Trigger ERROR hook (reached when both methods fail or backup can't be attempted)
        if not args.no_hooks:
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
        stop_reason = "launch_error"
        returncode = 1
        return 1

    except Exception as e:
        error_msg = f"Unexpected error launching agent: {e}"
        print(f"Error launching agent: {e}", file=sys.stderr)
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
        if not args.no_hooks:
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
        stop_reason = "launch_error"
        returncode = 1
        return 1

    finally:
        # Trigger AGENT_STOP hook in finally block (unless disabled)
        if not args.no_hooks and not args.no_session_end_hook:
            trigger_hook_sync(
                HookEvent.AGENT_STOP,
                HookContext(
                    event=HookEvent.AGENT_STOP,
                    instance_id=instance_id,
                    session_id=session_id,
                    client_type="cli",
                    client_id="standalone",
                    metadata={
                        "backend": backend,
                        "cwd": os.getcwd(),
                        "idle_detected": locals().get("idle_detected", False),
                        "reason": locals().get("stop_reason", "process_exit"),
                        "returncode": locals().get("returncode", 0),
                    },
                ),
                hook_debug=args.hook_debug,
            )
