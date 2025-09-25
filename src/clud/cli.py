"""Minimal CLI entry point for clud - routes to appropriate agent modules."""

import subprocess
import sys

from .agent_foreground import ConfigError as ForegroundConfigError
from .agent_foreground import ValidationError as ForegroundValidationError
from .agent_foreground import handle_login
from .cli_args import AgentMode, parse_router_args
from .task import handle_task_command


def handle_lint_command() -> int:
    """Handle the --lint command by running clud with a message to run codeup linting."""
    lint_prompt = "run codeup --lint --dry-run, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"

    try:
        # Run clud with the lint message using current Python and module
        result = subprocess.run(
            [sys.executable, "-m", "clud", "-m", lint_prompt],
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: Python interpreter not found.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud: {e}", file=sys.stderr)
        return 1


def handle_test_command() -> int:
    """Handle the --test command by running clud with a message to run codeup testing."""
    test_prompt = "run codeup --test --dry-run, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"

    try:
        # Run clud with the test message using current Python and module
        result = subprocess.run(
            [sys.executable, "-m", "clud", "-m", test_prompt],
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: Python interpreter not found.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud: {e}", file=sys.stderr)
        return 1


def handle_fix_command(url: str | None = None) -> int:
    """Handle the --fix command by running clud with a message to run both linting and testing."""
    if url and is_github_url(url):
        # Generate GitHub-specific prompt
        fix_prompt = generate_github_fix_prompt(url)
    else:
        # Default fix prompt
        fix_prompt = (
            "run `codeup --lint --dry-run` upto 5 times, fixing on each time or until it passes. "
            "and if it succeed then run `codeup --test --dry-run` upto 5 times, fixing each time until it succeeds. "
            "Finally run `codeup --lint --dry-run` and fix until it passes (upto 5 times) then halt. "
            "If you run into a locked file then try two times, same with misc system error. Else halt."
        )

    try:
        # Run clud with the fix message using current Python and module
        result = subprocess.run(
            [sys.executable, "-m", "clud", "-m", fix_prompt],
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: Python interpreter not found.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud: {e}", file=sys.stderr)
        return 1


def is_github_url(url: str) -> bool:
    """Check if the URL is a GitHub URL."""
    return url.startswith(("https://github.com/", "http://github.com/"))


def generate_github_fix_prompt(url: str) -> str:
    """Generate a prompt for fixing issues based on a GitHub URL."""
    base_fix_instructions = (
        "run `codeup --lint --dry-run` upto 5 times, fixing on each time or until it passes. "
        "and if it succeed then run `codeup --test --dry-run` upto 5 times, fixing each time until it succeeds. "
        "Finally run `codeup --lint --dry-run` and fix until it passes (upto 5 times) then halt. "
        "If you run into a locked file then try two times, same with misc system error. Else halt."
    )

    github_prompt = f"""First, download the logs from the GitHub URL: {url}
Use the `gh` command if available (e.g., `gh run view <run_id> --log` for workflow runs, or `gh pr view <pr_number>` for pull requests).
If `gh` is not available, use other means such as curl or web requests to fetch the relevant information from the GitHub API or page content.
Parse the logs to understand what issues need to be fixed.

Then proceed with the fix process:
{base_fix_instructions}"""

    return github_prompt


def main(args: list[str] | None = None) -> int:
    """Main entry point for clud - simplified router."""
    try:
        # Parse router arguments to determine mode and special commands
        router_args = parse_router_args(args)

        # Handle help first
        if router_args.help:
            print("clud - Claude-powered development container")
            print("Usage: clud [fg|bg] [options...]")
            print()
            print("Modes:")
            print("  fg    Run in foreground mode (Claude Code directly, default)")
            print("  bg    Run in background mode (Docker container)")
            print()
            print("Special commands:")
            print("  --login           Configure API key for Claude")
            print("  --task PATH       Open task file in editor")
            print("  --lint           Run global linting with codeup")
            print("  --test           Run tests with codeup")
            print("  --fix [URL]      Fix linting issues and run tests (optionally from GitHub URL)")
            print("  -h, --help       Show this help")
            print()
            print("For mode-specific options, use: clud <mode> --help")
            return 0

        # Handle special commands that don't require agents
        if router_args.login:
            return handle_login()

        if router_args.task:
            return handle_task_command(router_args.task)

        if router_args.lint:
            return handle_lint_command()

        if router_args.test:
            return handle_test_command()

        if router_args.fix:
            return handle_fix_command(router_args.fix_url)

        # Route to appropriate agent
        if router_args.mode == AgentMode.BACKGROUND:
            # Import background agent and pass remaining args
            from .agent_background import main as bg_main

            # Pass raw args to background agent's main function
            result = bg_main(router_args.remaining_args)
            return result if result is not None else 0
        else:
            # Foreground mode - import and run foreground agent
            from .agent_foreground import main as fg_main

            return fg_main(router_args.remaining_args)

    except (ForegroundValidationError, ForegroundConfigError) as e:
        print(f"Error: {e}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("\nOperation cancelled.", file=sys.stderr)
        return 2
    except Exception as e:
        # Handle other common exceptions that might come from agents
        error_msg = str(e)
        if "Docker" in error_msg or "docker" in error_msg:
            print(f"Docker error: {e}", file=sys.stderr)
            return 3
        elif "Config" in error_msg or "config" in error_msg:
            print(f"Configuration error: {e}", file=sys.stderr)
            return 4
        else:
            print(f"Unexpected error: {e}", file=sys.stderr)
            return 1


if __name__ == "__main__":
    sys.exit(main())
