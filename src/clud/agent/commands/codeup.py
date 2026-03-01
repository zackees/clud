"""CodeUp command handlers for clud agent."""

import sys

from clud.agent.lint_runner import _check_agent_artifacts
from clud.agent.prompts import UP_CODEUP_STEP, UP_PROMPT
from clud.agent.subprocess import run_clud_subprocess
from clud.agent.task_manager import _print_red_banner


def handle_codeup_command(commit_message: str | None = None) -> int:
    """Handle the 'clud up' command by running lint, test, cleanup, and codeup."""
    # Check for agent task artifacts first
    if not _check_agent_artifacts():
        return 1  # User aborted

    # Run git pre-check using CodeUp API
    from clud.git_precheck import run_two_phase_precheck

    print("Running git pre-check before agent invocation...", file=sys.stderr)
    try:
        result = run_two_phase_precheck(verbose=True)
        if not result.success:
            _print_red_banner("GIT PRE-CHECK FAILED")
            print(f"Error: {result.error_message}", file=sys.stderr)
            return 1
    except Exception as e:
        print(f"Warning: Error running git pre-check: {e}", file=sys.stderr)

    # If user provided a message, skip the auto-summary step
    if commit_message:
        prompt = UP_PROMPT.replace(
            UP_CODEUP_STEP,
            f'6. Once everything passes and is clean, run:\n   codeup -m "{commit_message}"\n   (codeup is a global command installed on the system)',
        )
    else:
        prompt = UP_PROMPT

    return run_clud_subprocess(prompt, use_print_flag=True)


def handle_codeup_publish_command(commit_message: str | None = None) -> int:
    """Handle the 'clud up -p' command by running lint, test, cleanup, and codeup -p."""
    # Check for agent task artifacts first
    if not _check_agent_artifacts():
        return 1  # User aborted

    # Run git pre-check using CodeUp API
    from clud.git_precheck import run_two_phase_precheck

    print("Running git pre-check before agent invocation...", file=sys.stderr)
    try:
        result = run_two_phase_precheck(verbose=True)
        if not result.success:
            _print_red_banner("GIT PRE-CHECK FAILED")
            print(f"Error: {result.error_message}", file=sys.stderr)
            return 1
    except Exception as e:
        print(f"Warning: Error running git pre-check: {e}", file=sys.stderr)

    # Build the publish variant of the prompt
    codeup_cmd = f'codeup -m "{commit_message}" -p' if commit_message else "codeup -p"

    prompt = UP_PROMPT.replace(
        UP_CODEUP_STEP,
        f"6. Once everything passes and is clean, run:\n   {codeup_cmd}\n   (codeup is a global command installed on the system)",
    )

    return run_clud_subprocess(prompt, use_print_flag=True)
