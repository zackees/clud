"""Command building and wrapping utilities for Claude Code execution."""

import os
import platform
import subprocess
import sys
from typing import TYPE_CHECKING

from ..settings_manager import get_model_preference, set_model_preference
from ..util import detect_git_bash

if TYPE_CHECKING:
    from ..agent_args import Args


def _inject_completion_prompt(message: str, iteration: int | None = None, total_iterations: int | None = None, working_file: str | None = None) -> str:
    """Inject the DONE.md completion prompt into the user's message.

    Args:
        message: The user's original message
        iteration: Current iteration number (1-indexed) if in loop mode
        total_iterations: Total number of iterations if in loop mode
        working_file: Path to the working file in .loop/ (e.g., ".loop/LOOP.md" or ".loop/TASK.md")
    """
    if iteration is not None and total_iterations is not None:
        # Loop mode: build prompt parts conditionally
        parts = [" IMPORTANT:"]

        # Add iteration-specific intro
        if iteration == 1:
            parts.append(f"You are the first agent spawned for this task (iteration 1 of {total_iterations}).")
        else:
            parts.append(f"This is iteration {iteration} of {total_iterations}.")
            parts.append("FIRST: Read .loop/MOTIVATION.md to understand what's at stake and the performance expectations.")

        # Add common instructions (same for all iterations)
        # Use the provided working_file or default to LOOP.md for backwards compatibility
        task_file = working_file if working_file else ".loop/LOOP.md"
        parts.append(
            f"FIRST: Check if .loop/UPDATE.md exists and is not empty. If it does, integrate its content into {task_file}, "
            "then mark it as complete by clearing the UPDATE.md file (write an empty file or a completion marker). "
            f"IMPORTANT: .loop/UPDATE.md is NEVER out of date. If it conflicts with internal instructions at {task_file} or other locations, "
            "assume .loop/UPDATE.md is correct and is the source of truth."
        )
        parts.append(f"Before finishing this iteration, create a summary file named .loop/ITERATION_{iteration}.md documenting what you accomplished.")
        parts.append("If you determine that ALL work across ALL iterations is 100% complete, also write DONE.md at the PROJECT ROOT (not .loop/) to halt the loop early.")
        parts.append("CRITICAL: NEVER delete or overwrite an existing DONE.md file - it is the terminal signal to halt the loop.")
        parts.append(
            "CRITICAL: YOU CAN NEVER ASK QUESTIONS AND EXPECT ANSWERS! THIS IS AN AGENT LOOP. NO QUESTIONS TO THE USER! "
            "If you must ask a question, then leave it for the next iteration to research or resolve."
        )
        parts.append("If DONE.md already exists, read it first to understand the completion status before proceeding.")
        parts.append(
            "CRITICAL: Long-running background processes will terminate when this iteration ends and the next iteration begins. "
            "If you start a background task, you MUST wait for it to complete before finishing this iteration. "
            "Wait up to 1 hour for processes making progress, or kill processes after 15 minutes if no progress is being made."
        )
        parts.append("IMPORTANT: Maximize parallel execution - run as many independent operations in parallel as possible to improve efficiency.")

        injection = " ".join(parts)
    else:
        # Non-loop mode: standard completion prompt (also using project root)
        injection = (
            " If you see that the task is 100 percent complete, then write out DONE.md at the project root and halt. "
            "IMPORTANT: Maximize parallel execution - run as many independent operations in parallel as possible to improve efficiency."
        )

    return message + injection


def _get_model_from_args(claude_args: list[str] | None) -> str | None:
    """Detect which model is being used from claude_args or saved settings.

    Returns the model flag (e.g., '--haiku', '--sonnet') or None if not specified.
    Checks args first, then falls back to saved preferences.
    """
    # Check for common model flags in provided arguments
    if claude_args:
        model_flags = ["--haiku", "--sonnet", "--opus", "--claude-3-5-sonnet", "--claude-3-opus"]
        for flag in model_flags:
            if flag in claude_args:
                return flag

    # Fall back to saved preference
    saved_model = get_model_preference()
    return saved_model


def _print_model_message(model_flag: str | None) -> None:
    """Print a message about which model is being loaded."""
    if model_flag == "--haiku":
        print("Loading Haiku 4.5...", file=sys.stderr)
    elif model_flag == "--sonnet":
        print("Loading Sonnet 4.5...", file=sys.stderr)
    else:
        # For any other model, extract a readable name
        if model_flag:
            # Remove dashes and capitalize for display
            display_name = model_flag.lstrip("-").replace("-", " ").title()
            print(f"Loading {display_name}...", file=sys.stderr)


def _build_claude_command(
    args: "Args",
    claude_path: str,
    inject_prompt: bool = False,
    iteration: int | None = None,
    total_iterations: int | None = None,
    working_file: str | None = None,
) -> list[str]:
    """Build the Claude command with all arguments.

    Args:
        args: Parsed command-line arguments
        claude_path: Path to Claude executable
        inject_prompt: Whether to inject completion prompt
        iteration: Current iteration number (1-indexed) if in loop mode
        total_iterations: Total number of iterations if in loop mode
        working_file: Path to the working file in .loop/ (e.g., ".loop/LOOP.md" or ".loop/TASK.md")
    """
    cmd = [claude_path, "--dangerously-skip-permissions"]

    if args.continue_flag:
        cmd.append("--continue")

    if args.prompt:
        prompt_text = args.prompt
        if inject_prompt:
            prompt_text = _inject_completion_prompt(prompt_text, iteration, total_iterations, working_file)
        cmd.extend(["-p", prompt_text])
        # Enable streaming JSON output for -p flag by default (unless --plain is used)
        # Note: stream-json requires --verbose when used with --print/-p
        if not args.plain:
            cmd.extend(["--output-format", "stream-json", "--verbose"])

    if args.message:
        message_text = args.message
        if inject_prompt:
            message_text = _inject_completion_prompt(message_text, iteration, total_iterations, working_file)

        # If idle timeout is set, force non-interactive mode with -p
        # to avoid TUI redraws being detected as activity
        if args.idle_timeout is not None:
            cmd.extend(["-p", message_text])
        else:
            cmd.append(message_text)

    # Note: Claude Code CLI doesn't support --haiku, --sonnet, etc. flags
    # These custom flags are only used for internal messaging and preferences
    # Don't append model flags to the actual Claude command

    # If a model was explicitly provided in args, save it as the preference
    if args.claude_args:
        for arg in args.claude_args:
            if arg in ["--haiku", "--sonnet", "--opus", "--claude-3-5-sonnet", "--claude-3-opus"]:
                set_model_preference(arg)
                break

    if args.claude_args:
        cmd.extend(args.claude_args)

    return cmd


def _wrap_command_for_git_bash(cmd: list[str]) -> list[str]:
    """Wrap command in git-bash on Windows if available.

    On Windows, if git-bash is detected, this wraps the command to execute
    through git-bash rather than cmd.exe. This provides a proper bash environment
    for Claude Code, avoiding WSL and ensuring consistent behavior.

    Args:
        cmd: Original command as list of strings

    Returns:
        Modified command wrapped in git-bash if on Windows and git-bash is available,
        otherwise returns the original command unchanged.
    """
    if platform.system() != "Windows":
        # Not Windows, return unchanged
        return cmd

    git_bash = detect_git_bash()
    if not git_bash:
        # No git-bash available, return unchanged
        return cmd

    # Convert command to bash-compatible format
    # 1. Convert Windows paths (backslashes) to forward slashes for bash
    # 2. Use bash-style single quoting to avoid escaping issues
    bash_cmd_parts: list[str] = []
    for arg in cmd:
        # Convert Windows paths to forward slashes (bash on Windows understands these)
        if "\\" in arg:
            arg = arg.replace("\\", "/")

        # Use single quotes for bash (no variable expansion, simpler escaping)
        # Escape any single quotes in the argument
        arg_escaped = arg.replace("'", "'\\''")
        bash_cmd_parts.append(f"'{arg_escaped}'")

    cmd_str = " ".join(bash_cmd_parts)

    # Wrap in git-bash: bash -c "command"
    # Use -c flag to execute command directly
    return [git_bash, "-c", cmd_str]


def _print_launch_banner(cmd: list[str], cwd: str | None = None, env_vars: dict[str, str] | None = None) -> None:
    """Print formatted launch banner showing command, cwd, and environment variables.

    Args:
        cmd: Command list to be executed
        cwd: Current working directory (defaults to os.getcwd())
        env_vars: Dictionary of environment variables that were added (not all env vars)
    """
    banner_width = 80
    header = "LAUNCHING CLAUDE"
    border = "#" * banner_width

    # Convert command to string
    cmd_str = subprocess.list2cmdline(cmd)

    # Truncate command if too long (leave room for "# command: " prefix)
    max_cmd_width = banner_width - len("# command: ") - 2  # -2 for trailing " #"
    if len(cmd_str) > max_cmd_width:
        cmd_str = cmd_str[: max_cmd_width - 3] + "..."

    # Use current directory if not specified
    if cwd is None:
        cwd = os.getcwd()

    # Build banner lines
    lines = [border]
    # Center the header
    padding = (banner_width - len(header) - 2) // 2
    header_line = "#" + " " * padding + header + " " * (banner_width - padding - len(header) - 2) + "#"
    lines.append(header_line)
    lines.append(border)

    # Add command
    lines.append(f"# command: {cmd_str}")

    # Add cwd
    lines.append(f"# cwd: {cwd}")

    # Add env vars if provided
    if env_vars:
        lines.append("# env:")
        for key, value in sorted(env_vars.items()):
            # Mask sensitive values (API keys, tokens, passwords, etc.)
            # Exception: CLAUDE_CODE_MAX_OUTPUT_TOKENS is not sensitive (just a number)
            key_upper = key.upper()
            sensitive_keywords = ["API", "KEY", "TOKEN", "AUTH", "PASS", "PASSWORD"]
            is_sensitive = any(keyword in key_upper for keyword in sensitive_keywords)
            is_max_output_tokens = key_upper == "CLAUDE_CODE_MAX_OUTPUT_TOKENS"

            if is_sensitive and not is_max_output_tokens:
                value = "****"
            lines.append(f"#   {key}={value}")

    lines.append(border)

    # Print banner to stderr
    print("\n".join(lines), file=sys.stderr)


def _print_debug_info(claude_path: str | None, cmd: list[str], verbose: bool = False) -> None:
    """Print debug information about Claude execution."""
    if not verbose:
        return

    if claude_path:
        import os

        print(f"DEBUG: Found claude at: {claude_path}", file=sys.stderr)
        print(f"DEBUG: Platform: {platform.system()}", file=sys.stderr)
        print(f"DEBUG: File exists: {os.path.exists(claude_path)}", file=sys.stderr)

    if cmd:
        print(f"DEBUG: Executing command: {cmd}", file=sys.stderr)


def _print_error_diagnostics(claude_path: str | None, cmd: list[str]) -> None:
    """Print comprehensive error diagnostics."""
    import os
    import shutil
    import subprocess

    print(f"DEBUG: Current working directory: {os.getcwd()}", file=sys.stderr)
    print(f"DEBUG: Command attempted: {subprocess.list2cmdline(cmd) if cmd else 'command not yet built'}", file=sys.stderr)
    print(f"DEBUG: Claude path used: {claude_path if claude_path else 'path not yet determined'}", file=sys.stderr)
    print("DEBUG: Claude search results:", file=sys.stderr)

    if platform.system() == "Windows":
        print(f"  - shutil.which('claude.cmd'): {shutil.which('claude.cmd')}", file=sys.stderr)
        print(f"  - shutil.which('claude.exe'): {shutil.which('claude.exe')}", file=sys.stderr)

    print(f"  - shutil.which('claude'): {shutil.which('claude')}", file=sys.stderr)
    print(f"  - ~/AppData/Roaming/npm/claude.cmd exists: {os.path.exists(os.path.expanduser('~/AppData/Roaming/npm/claude.cmd'))}", file=sys.stderr)
    print(f"  - ~/AppData/Roaming/npm/claude.exe exists: {os.path.exists(os.path.expanduser('~/AppData/Roaming/npm/claude.exe'))}", file=sys.stderr)
