"""Lint command handler for clud agent."""

from clud.agent.prompts import LINT_PROMPT
from clud.agent.subprocess import run_clud_subprocess


def handle_lint_command() -> int:
    """Handle the --lint command by running clud with a message to run lint-test."""
    return run_clud_subprocess(LINT_PROMPT)
