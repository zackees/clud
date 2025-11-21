"""Init loop command handler for clud agent."""

from clud.agent.subprocess import run_clud_subprocess


def handle_init_loop_command() -> int:
    """Handle the --init-loop command by running clud to create a LOOP.md index file."""
    init_loop_prompt = (
        "Look at checked-out *.md files and ones not added to the repo yet (use git status). "
        "Then write out LOOP.md which will contain an index of md files to consult. "
        "The index should list each markdown file with a brief description of its contents. "
        "Format LOOP.md as a reference guide for loop mode iterations."
    )
    return run_clud_subprocess(init_loop_prompt, use_print_flag=True)
