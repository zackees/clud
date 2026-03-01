"""Fix command handler for clud agent."""

from clud.agent.github import generate_github_fix_prompt, is_github_url
from clud.agent.prompts import FIX_PROMPT
from clud.agent.subprocess import run_clud_subprocess


def handle_fix_command(url: str | None = None) -> int:
    """Handle the fix command by running clud with a message to fix linting and tests."""
    fix_prompt = generate_github_fix_prompt(url) if url and is_github_url(url) else FIX_PROMPT
    return run_clud_subprocess(fix_prompt)
