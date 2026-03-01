"""Fix command handler for clud agent."""

from clud.agent.github import generate_github_fix_prompt, is_github_url
from clud.agent.subprocess import run_clud_subprocess

FIX_PROMPT = (
    "Look for linting like ./lint, or npm or python, choose the most likely one, "
    "then look for unit tests like ./test or pytest or npm test, run the most likely one. "
    "For each stage fix until it works, rerunning it until it does."
)


def handle_fix_command(url: str | None = None) -> int:
    """Handle the fix command by running clud with a message to fix linting and tests."""
    fix_prompt = generate_github_fix_prompt(url) if url and is_github_url(url) else FIX_PROMPT
    return run_clud_subprocess(fix_prompt)
