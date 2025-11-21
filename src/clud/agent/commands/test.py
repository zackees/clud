"""Test command handler for clud agent."""

from clud.agent.subprocess import run_clud_subprocess


def handle_test_command() -> int:
    """Handle the --test command by running clud with a message to run lint-test."""
    test_prompt = "run lint-test, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"
    return run_clud_subprocess(test_prompt)
