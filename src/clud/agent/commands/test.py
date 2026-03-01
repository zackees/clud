"""Test command handler for clud agent."""

from clud.agent.prompts import TEST_PROMPT
from clud.agent.subprocess import run_clud_subprocess


def handle_test_command() -> int:
    """Handle the --test command by running clud with a message to run lint-test."""
    return run_clud_subprocess(TEST_PROMPT)
