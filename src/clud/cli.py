"""Minimal CLI entry point for clud - routes to appropriate agent modules."""

import subprocess
import sys

from .agent.foreground import ConfigError as ForegroundConfigError
from .agent.foreground import ValidationError as ForegroundValidationError
from .agent.foreground import handle_login, save_telegram_credentials
from .cli_args import AgentMode, parse_router_args
from .task import handle_task_command


def run_clud_subprocess(
    prompt: str,
    use_print_flag: bool = False,
    additional_args: list[str] | None = None,
) -> int:
    """Run clud as a subprocess with the given prompt.

    Args:
        prompt: The prompt/message to pass to clud
        use_print_flag: If True, uses -p flag; if False, uses -m flag
        additional_args: Optional additional command-line arguments

    Returns:
        Exit code from clud subprocess
    """
    try:
        cmd = [sys.executable, "-m", "clud"]

        # Add prompt with appropriate flag
        flag = "-p" if use_print_flag else "-m"
        cmd.extend([flag, prompt])

        # Add any additional arguments
        if additional_args:
            cmd.extend(additional_args)

        result = subprocess.run(
            cmd,
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: Python interpreter not found.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud subprocess: {e}", file=sys.stderr)
        return 1


def handle_lint_command() -> int:
    """Handle the --lint command by running clud with a message to run codeup linting."""
    lint_prompt = "run codeup --lint --dry-run, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"
    return run_clud_subprocess(lint_prompt)


def handle_test_command() -> int:
    """Handle the --test command by running clud with a message to run codeup testing."""
    test_prompt = "run codeup --test --dry-run, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"
    return run_clud_subprocess(test_prompt)


def handle_codeup_command() -> int:
    """Handle the --codeup command by running clud with a message to run the global codeup command."""
    codeup_prompt = (
        "run the global command codeup normally through the shell (it's a global command installed on the system), "
        "if it returns 0, halt, if it fails then read the output logs and apply the fixes. "
        "Run upto 5 times before giving up, else halt."
    )
    return run_clud_subprocess(codeup_prompt)


def handle_codeup_publish_command() -> int:
    """Handle the --codeup-publish command by running clud with a message to run codeup -p."""
    codeup_publish_prompt = (
        "run the global command codeup -p normally through the shell (it's a global command installed on the system), "
        "if it returns 0, halt, if it fails then read the output logs and apply the fixes. "
        "Run upto 5 times before giving up, else halt."
    )
    return run_clud_subprocess(codeup_publish_prompt)


def handle_kanban_command() -> int:
    """Handle the --kanban command by setting up and running vibe-kanban."""
    from .kanban_manager import setup_and_run_kanban

    try:
        return setup_and_run_kanban()
    except Exception as e:
        print(f"Error running kanban: {e}", file=sys.stderr)
        return 1


def handle_telegram_command(token: str | None = None) -> int:
    """Handle the --telegram/-tg command by starting Telegram Web App server.

    Args:
        token: Optional bot token to save before launching

    Returns:
        Exit code
    """
    from .webapp.server import run_server

    try:
        # Save token if provided
        if token:
            print("Saving Telegram bot token...")
            try:
                # Save with empty chat_id - will be auto-detected from Web App
                save_telegram_credentials(token, "")
                print("✓ Token saved successfully")
                print("  Chat ID will be auto-detected when you open the Web App in Telegram\n")
            except Exception as e:
                print(f"Warning: Could not save token: {e}", file=sys.stderr)
                print("Continuing to launch Web App...\n", file=sys.stderr)

        print("Starting Telegram Web App server...")
        return run_server()
    except Exception as e:
        print(f"Error running Telegram Web App: {e}", file=sys.stderr)
        return 1


def handle_webui_command(port: int | None = None) -> int:
    """Handle the --webui command by launching Web UI server."""
    from .webui.server import run_server

    try:
        print("Starting Claude Code Web UI...")
        return run_server(port)
    except Exception as e:
        print(f"Error running Web UI: {e}", file=sys.stderr)
        return 1


def handle_init_loop_command() -> int:
    """Handle the --init-loop command by running clud to create a LOOP.md index file."""
    init_loop_prompt = (
        "Look at checked-out *.md files and ones not added to the repo yet (use git status). "
        "Then write out LOOP.md which will contain an index of md files to consult. "
        "The index should list each markdown file with a brief description of its contents. "
        "Format LOOP.md as a reference guide for loop mode iterations."
    )
    return run_clud_subprocess(init_loop_prompt, use_print_flag=True)


def handle_code_command(port: int | None = None) -> int:
    """Handle the --code command by launching code-server via npx."""
    import os
    import socket
    import time
    import webbrowser

    def is_port_available(port: int) -> bool:
        """Check if a port is available for binding."""
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
                sock.bind(("localhost", port))
                return True
        except OSError:
            return False

    def find_available_port(start_port: int = 8080) -> int:
        """Find an available port starting from start_port."""
        for port_candidate in range(start_port, start_port + 100):
            if is_port_available(port_candidate):
                return port_candidate
        raise RuntimeError(f"No available ports found starting from {start_port}")

    # Find available port
    if port is None:
        port = find_available_port(8080)
    else:
        # User specified a port, check if it's available
        if not is_port_available(port):
            print(f"⚠️  Port {port} is not available, finding alternative...")
            port = find_available_port(port)

    # Get current working directory
    workspace = os.getcwd()

    print(f"🚀 Launching code-server on port {port}...")
    print(f"📁 Workspace: {workspace}")
    print()

    # Build npx command
    cmd = [
        "npx",
        "code-server",
        "--bind-addr",
        f"0.0.0.0:{port}",
        "--auth",
        "none",
        "--disable-telemetry",
        workspace,
    ]

    # Schedule browser opening
    def open_browser_delayed():
        time.sleep(3)
        url = f"http://localhost:{port}"
        print(f"\n🌐 Opening browser to {url}")
        try:
            webbrowser.open(url)
            print(f"✓ VS Code server is now accessible at {url}")
            print("\nPress Ctrl+C to stop the server")
        except Exception as e:
            print(f"Could not open browser automatically: {e}")
            print(f"Please open {url} in your browser")

    import threading

    browser_thread = threading.Thread(target=open_browser_delayed, daemon=True)
    browser_thread.start()

    # Run code-server
    try:
        result = subprocess.run(cmd, check=False)
        return result.returncode
    except FileNotFoundError:
        print("Error: npx not found. Make sure Node.js is installed.", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\n\nStopping code-server...", file=sys.stderr)
        return 0
    except Exception as e:
        print(f"Error running code-server: {e}", file=sys.stderr)
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
    return run_clud_subprocess(fix_prompt)


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
            print("clud - Claude Code in YOLO mode")
            print("Usage: clud [options...]")
            print()
            print("Special modes:")
            print("  fix   Fix linting and test issues (with optional GitHub URL)")
            print("  up    Run global codeup command with auto-fix")
            print()
            print("Special commands:")
            print("  --login              Configure API key for Claude")
            print("  --task PATH          Open task file in editor")
            print("  --code [PORT]        Launch code-server in browser (default port: 8080)")
            print("  --lint               Run global linting with codeup")
            print("  --test               Run tests with codeup")
            print("  --codeup             Run global codeup command with auto-fix (up to 5 retries)")
            print("  --codeup-publish     Run global codeup -p command with auto-fix (up to 5 retries)")
            print("  --codeup-p           Alias for --codeup-publish")
            print("  --fix [URL]          Fix linting issues and run tests (optionally from GitHub URL)")
            print("  --init-loop          Create LOOP.md index from existing markdown files")
            print("  --kanban             Launch vibe-kanban board (installs Node 22 if needed)")
            print("  --telegram, -tg      Start Telegram Web App server for bot integration")
            print("  --webui [PORT]       Launch Claude Code Web UI in browser (default port: 8888)")
            print("  --track              Enable agent tracking with local daemon")
            print("  -h, --help           Show this help")
            print()
            print("Default: Run Claude Code with --dangerously-skip-permissions")
            return 0

        # Handle special commands that don't require agents
        if router_args.login:
            return handle_login()

        if router_args.task is not None:
            return handle_task_command(router_args.task)

        if router_args.lint:
            return handle_lint_command()

        if router_args.test:
            return handle_test_command()

        if router_args.codeup:
            return handle_codeup_command()

        if router_args.codeup_publish:
            return handle_codeup_publish_command()

        if router_args.kanban:
            return handle_kanban_command()

        if router_args.telegram:
            return handle_telegram_command(router_args.telegram_token)

        if router_args.webui:
            return handle_webui_command(router_args.webui_port)

        if router_args.code:
            return handle_code_command(router_args.code_port)

        if router_args.fix:
            return handle_fix_command(router_args.fix_url)

        if router_args.init_loop:
            return handle_init_loop_command()

        # Route to appropriate mode handler
        if router_args.mode == AgentMode.FIX:
            # Extract optional URL from remaining args
            fix_url = router_args.remaining_args[0] if router_args.remaining_args else None
            return handle_fix_command(fix_url)
        elif router_args.mode == AgentMode.UP:
            return handle_codeup_command()
        else:
            # Default mode - run foreground agent
            # If --track is enabled, set up tracking before launching agent
            if router_args.track:
                from .agent.tracking import create_tracker

                # Get command from remaining args
                command = " ".join(router_args.remaining_args) if router_args.remaining_args else "claude code"
                tracker = create_tracker(command)

                exit_code = 1  # Default exit code in case of exception
                try:
                    from .agent.foreground import main as fg_main

                    exit_code = fg_main(router_args.remaining_args)
                finally:
                    tracker.stop(exit_code)

                return exit_code
            else:
                from .agent.foreground import main as fg_main

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
        if "Config" in error_msg or "config" in error_msg:
            print(f"Configuration error: {e}", file=sys.stderr)
            return 4
        else:
            print(f"Unexpected error: {e}", file=sys.stderr)
            return 1


if __name__ == "__main__":
    sys.exit(main())
