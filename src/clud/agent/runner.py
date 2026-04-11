"""Core agent execution logic for Claude Code."""

import logging
import os
import re
import subprocess
import sys
import traceback
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

from running_process import RunningProcess

from ..claude_installer import prompt_install_claude
from ..hooks import HookContext, HookEvent
from ..hooks.claude_compat import get_codex_stop_hook_idle_timeout, load_claude_compat_hooks
from ..hooks.command import run_command_hook
from ..json_formatter import StreamJsonFormatter, create_formatter_callback
from ..skill_installer import install_skills, needs_install
from ..util import handle_keyboard_interrupt
from .arg_translation import to_agent_args
from .backends.registry import get_backend
from .claude_finder import _find_claude_path
from .command_builder import (
    _get_effective_backend,
    _persist_backend_selection,
    _print_debug_info,
    _print_error_diagnostics,
    _print_launch_banner,
    _wrap_command_for_git_bash,
)
from .hooks import HookRegistrationSummary, register_hooks_from_config, trigger_hook_sync
from .loop_executor import _run_loop
from .process_launcher import run_claude_process
from .prompts import LOOP_PROMPT_TEMPLATE
from .subprocess import _execute_command

if TYPE_CHECKING:
    from ..agent_args import Args
    from .backends.base import BackendAdapter
    from .interfaces import AgentArgs
from .user_input import _prompt_for_message

# Initialize logger
logger = logging.getLogger(__name__)

_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;]*[a-zA-Z]")
_CONTROL_CHAR_RE = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")


def _sanitize_hook_output(text: str) -> str:
    """Strip ANSI escape codes and control characters from hook output."""
    text = _ANSI_ESCAPE_RE.sub("", text)
    text = _CONTROL_CHAR_RE.sub("", text)
    return text.strip()


def _find_backend_executable(backend: str) -> str | None:
    """Find the executable for the selected backend."""
    if backend == "claude":
        return _find_claude_path()
    try:
        return get_backend(backend).find_executable()
    except KeyError:
        return None


def _handle_dry_run(args: "Args", backend: str, backend_adapter: "BackendAdapter", agent_args: "AgentArgs") -> int:
    """Handle --dry-run mode: print what would be executed and exit."""
    if args.loop_value is not None:
        loop_count = args.loop_count_override if args.loop_count_override is not None else 50
        working_file_path = ".loop/LOOP.md"
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

    plan = backend_adapter.build_launch_plan(agent_args)  # type: ignore[union-attr]
    print("Would execute:", " ".join([backend, *plan.argv]))
    return 0


def _handle_launch_error(
    error: Exception,
    cmd: list[str],
    claude_path: str | None,
    args: "Args",
    instance_id: str,
    session_id: str,
) -> int:
    """Handle OSError/Exception during agent launch with optional shell=True retry."""
    error_msg = f"Error launching agent: {error}"
    if cmd and claude_path:
        try:
            if args.verbose:
                winerror = getattr(error, "winerror", "N/A")
                errno = getattr(error, "errno", "N/A")
                print(f"DEBUG: Error {winerror or errno}, retrying with shell=True...", file=sys.stderr)
            return _execute_command(cmd, use_shell=True, verbose=args.verbose)
        except Exception as shell_error:
            print(f"Error launching agent: {error}", file=sys.stderr)
            errno = getattr(error, "errno", "N/A")
            winerror = getattr(error, "winerror", "N/A")
            print(f"DEBUG: Error details - errno: {errno}, winerror: {winerror}", file=sys.stderr)
            _print_error_diagnostics(claude_path, cmd)
            print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
            traceback.print_exc()
    else:
        print(f"Error launching agent: {error}", file=sys.stderr)
        errno = getattr(error, "errno", "N/A")
        winerror = getattr(error, "winerror", "N/A")
        print(f"DEBUG: Error details - errno: {errno}, winerror: {winerror}", file=sys.stderr)
        _print_error_diagnostics(claude_path, cmd)
        print("\nFull stack trace from original error:", file=sys.stderr)
        traceback.print_exc()

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

    hook_summary = HookRegistrationSummary()

    try:
        _persist_backend_selection(args)
        backend = _get_effective_backend(args)
        backend_adapter = get_backend(backend)
        agent_args = to_agent_args(args, resolved_backend=backend, cwd=os.getcwd())

        # Handle dry-run mode early (before API key check)
        if args.dry_run:
            return _handle_dry_run(args, backend, backend_adapter, agent_args)

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

        # Determine Codex interactive mode for PTY idle-timeout path
        codex_interactive = backend == "codex" and plan.interactive

        # Load compat hooks for inline stop-hook handling (Codex only)
        compat_hooks = None
        if codex_interactive and not args.no_hooks:
            compat_hooks = load_claude_compat_hooks(cwd=Path.cwd())

        # Register hooks (after plan so we can control inline stop-hook registration)
        if not args.no_hooks:
            register_compat_stop = not (codex_interactive and compat_hooks is not None and compat_hooks.has_stop)
            hook_summary = register_hooks_from_config(
                hook_debug=args.hook_debug,
                cwd=Path.cwd(),
                register_compat_stop=register_compat_stop,
            )

        # Claude benefits from git-bash on Windows, but wrapping Codex breaks
        # its native interactive TUI input handling.
        if backend == "claude":
            cmd = _wrap_command_for_git_bash(cmd)

        # Debug TTY diagnostics
        if args.debug_tty:
            print(
                f"TTY DEBUG: stdin.isatty={sys.stdin.isatty()}, stdout.isatty={sys.stdout.isatty()}, stderr.isatty={sys.stderr.isatty()}",
                file=sys.stderr,
            )
            if codex_interactive:
                print(
                    f"TTY DEBUG: backend={backend}, launch_mode=interactive-pty-idle-default, codex_interactive_default_pty=True, git_bash_wrap_applied=False",
                    file=sys.stderr,
                )

        # Codex interactive requires a real terminal on stdin
        if codex_interactive and not sys.stdin.isatty():
            print("Error: Codex interactive mode requires a terminal on stdin", file=sys.stderr)
            return 2

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
        if effective_idle_timeout is None and codex_interactive:
            effective_idle_timeout = get_codex_stop_hook_idle_timeout()
            if hook_summary.has_post_execution_hooks:
                print(
                    f"Claude-compatible Stop hooks detected; using Codex idle timeout {effective_idle_timeout:.1f}s",
                    file=sys.stderr,
                )

        if effective_idle_timeout is not None:
            # Build inline stop-hook callback for the PTY idle path
            idle_callback: Callable[[], str | None] | None = None
            if compat_hooks and compat_hooks.has_stop:
                stop_context = HookContext(
                    event=HookEvent.POST_EXECUTION,
                    instance_id=instance_id,
                    session_id=session_id,
                    client_type="cli",
                    client_id="standalone",
                    message=user_message,
                )

                def _run_stop_hooks() -> str | None:
                    outputs: list[str] = []
                    for spec in compat_hooks.stop:  # type: ignore[union-attr]
                        hook_result = run_command_hook(spec, stop_context)
                        if not hook_result.failed and hook_result.stdout:
                            outputs.append(hook_result.stdout)
                    if outputs:
                        return _sanitize_hook_output("\n".join(outputs))
                    return None

                idle_callback = _run_stop_hooks

            pty_result = run_claude_process(
                cmd,
                idle_timeout=effective_idle_timeout,
                propagate_keyboard_interrupt=False,
                on_idle=idle_callback,
            )
            if isinstance(pty_result, int):
                returncode = pty_result
            else:
                idle_detected = pty_result.idle_detected
                returncode = pty_result.returncode
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
            interactive_result = run_claude_process(cmd, propagate_keyboard_interrupt=False)
            returncode = interactive_result if isinstance(interactive_result, int) else interactive_result.returncode

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
        return _handle_launch_error(e, cmd, claude_path, args, instance_id, session_id)

    except Exception as e:
        return _handle_launch_error(e, cmd, claude_path, args, instance_id, session_id)

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
