"""Init loop command handler for clud agent."""

from clud.agent.prompts import INIT_LOOP_PROMPT
from clud.agent.subprocess import run_clud_subprocess


def handle_init_loop_command() -> int:
    """Handle the --init-loop command by running clud to create a LOOP.md index file."""
    return run_clud_subprocess(INIT_LOOP_PROMPT, use_print_flag=True)
