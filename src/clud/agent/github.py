"""
GitHub URL handling for fix command.

This module provides utilities for detecting and processing GitHub URLs
in fix commands.
"""


def is_github_url(url: str) -> bool:
    """Check if the URL is a GitHub URL."""
    return url.startswith(("https://github.com/", "http://github.com/"))


def generate_github_fix_prompt(url: str) -> str:
    """Generate a prompt for fixing issues based on a GitHub URL."""
    base_fix_instructions = "run `lint-test` upto 5 times, fixing on each time or until it passes. If you run into a locked file then try two times, same with misc system error. Else halt."

    github_prompt = f"""First, download the logs from the GitHub URL: {url}
Use the `gh` command if available (e.g., `gh run view <run_id> --log` for workflow runs, or `gh pr view <pr_number>` for pull requests).
If `gh` is not available, use other means such as curl or web requests to fetch the relevant information from the GitHub API or page content.
Parse the logs to understand what issues need to be fixed.

Then proceed with the fix process:
{base_fix_instructions}"""

    return github_prompt
