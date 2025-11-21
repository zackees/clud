"""Fix command handler for clud agent."""

from clud.agent.github import generate_github_fix_prompt, is_github_url
from clud.agent.subprocess import run_clud_subprocess


def handle_fix_command(url: str | None = None) -> int:
    """Handle the --fix command by running clud with a message to run both linting and testing."""
    if url and is_github_url(url):
        # Generate GitHub-specific prompt
        fix_prompt = generate_github_fix_prompt(url)
    else:
        # Default fix prompt
        fix_prompt = "run `lint-test` upto 5 times, fixing on each time or until it passes. If you run into a locked file then try two times, same with misc system error. Else halt."
    return run_clud_subprocess(fix_prompt)
