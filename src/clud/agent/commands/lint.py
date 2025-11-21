"""Lint command handler for clud agent."""

from clud.agent.subprocess import run_clud_subprocess


def handle_lint_command() -> int:
    """Handle the --lint command by running clud with a message to run lint-test."""
    lint_prompt = "run lint-test, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"
    return run_clud_subprocess(lint_prompt)
