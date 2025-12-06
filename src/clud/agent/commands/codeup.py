"""CodeUp command handlers for clud agent."""

import sys

from clud.agent.lint_runner import _check_agent_artifacts
from clud.agent.subprocess import run_clud_subprocess
from clud.agent.task_manager import _print_red_banner


def handle_codeup_command() -> int:
    """Handle the --codeup command by running git pre-check first, then clud with a message to run the global codeup command."""
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

    # Now run the agent with the codeup prompt
    codeup_prompt = (
        "run the global command codeup normally through the shell (it's a global command installed on the system), "
        "wait for the tests to complete if necessary (sometimes tests take a long time with clud up), "
        "if it returns 0, halt, if it fails then read the output logs and apply the fixes. "
        "Run upto 5 times before giving up, else halt."
    )
    return run_clud_subprocess(codeup_prompt, use_print_flag=True)


def handle_codeup_publish_command() -> int:
    """Handle the --codeup-publish command by running git pre-check first, then clud with a message to run codeup -p."""
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

    # Now run the agent with the codeup -p prompt
    codeup_publish_prompt = (
        "run the global command codeup -p normally through the shell (it's a global command installed on the system), "
        "wait for the tests to complete if necessary (sometimes tests take a long time with clud up), "
        "if it returns 0, halt, if it fails then read the output logs and apply the fixes. "
        "Run upto 5 times before giving up, else halt."
    )
    return run_clud_subprocess(codeup_publish_prompt)
