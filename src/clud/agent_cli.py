"""Consolidated agent module - handles all Claude Code execution and special commands."""

import contextlib
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import threading
import time
import traceback
import webbrowser
from pathlib import Path
from typing import Any

from running_process import RunningProcess

from .agent.completion import detect_agent_completion
from .agent_args import AgentMode, Args, parse_args
from .json_formatter import StreamJsonFormatter, create_formatter_callback
from .output_filter import OutputFilter
from .secrets import get_credential_store
from .task import handle_task_command
from .telegram_bot import TelegramBot

# Get credential store once at module level
keyring = get_credential_store()


# Exception classes
class CludError(Exception):
    """Base exception for clud errors."""

    pass


class ValidationError(CludError):
    """User/validation error."""

    pass


class ConfigError(CludError):
    """Configuration error."""

    pass


# ============================================================================
# API Key Management
# ============================================================================


def validate_api_key(api_key: str | None) -> bool:
    """Validate API key format."""
    if not api_key:
        return False

    # Clean the API key
    api_key = api_key.strip()

    # Remove any BOM characters that might be present
    if api_key.startswith("\ufeff"):
        api_key = api_key[1:]

    # Basic validation: should start with sk-ant- and have reasonable length
    if not api_key.startswith("sk-ant-"):
        return False

    # Should be at least 20 characters (conservative minimum)
    return len(api_key) >= 20


def get_api_key_from_keyring(keyring_name: str) -> str | None:
    """Get API key from OS keyring or fallback credential store."""
    if keyring is None:
        raise ConfigError("No credential storage available. Install with: pip install keyring, keyrings.cryptfile, or cryptography")

    try:
        api_key = keyring.get_password("clud", keyring_name)
        if not api_key:
            raise ConfigError(f"No API key found in credential store for '{keyring_name}'")
        return api_key
    except Exception as e:
        raise ConfigError(f"Failed to retrieve API key from credential store: {e}") from e


def get_clud_config_dir() -> Path:
    """Get or create the .clud config directory."""
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(exist_ok=True)
    return config_dir


def save_api_key_to_config(api_key: str, key_name: str = "anthropic-api-key") -> None:
    """Save API key to .clud config directory."""
    try:
        config_dir = get_clud_config_dir()
        key_file = config_dir / f"{key_name}.key"

        # Write API key to file with restrictive permissions
        # Ensure no trailing newlines or spaces
        key_file.write_text(api_key.strip(), encoding="utf-8")

        # Set restrictive permissions (owner read/write only)
        if platform.system() != "Windows":
            key_file.chmod(0o600)
        else:
            # On Windows, try to set file as hidden
            try:
                import ctypes

                FILE_ATTRIBUTE_HIDDEN = 0x02
                ctypes.windll.kernel32.SetFileAttributesW(str(key_file), FILE_ATTRIBUTE_HIDDEN)
            except Exception:
                pass  # Not critical if hiding fails

    except Exception as e:
        raise ConfigError(f"Failed to save API key to config: {e}") from e


def load_api_key_from_config(key_name: str = "anthropic-api-key") -> str | None:
    """Load API key from .clud config directory."""
    try:
        config_dir = get_clud_config_dir()
        key_file = config_dir / f"{key_name}.key"

        if key_file.exists():
            # Read and thoroughly clean the API key
            api_key = key_file.read_text(encoding="utf-8").strip()
            # Remove any BOM characters that might be present on Windows
            if api_key.startswith("\ufeff"):
                api_key = api_key[1:]
            return api_key if api_key else None
        return None

    except Exception as e:
        # Log the error for debugging but don't crash
        print(f"Warning: Could not load API key from config: {e}", file=sys.stderr)
        return None


def handle_login() -> int:
    """Handle the --login command to configure API key."""
    print("Configure Claude API Key")
    print("-" * 40)

    # Check if we already have a saved key
    existing_key = load_api_key_from_config()
    if existing_key:
        print("An API key is already configured.")
        sys.stdout.flush()
        overwrite = input("Do you want to replace it? (y/N): ").strip().lower()
        if overwrite not in ["y", "yes"]:
            print("Keeping existing API key.")
            return 0

    # Prompt for new key
    while True:
        try:
            sys.stdout.flush()
            api_key = input("Please enter your Anthropic API key: ").strip()
            if not api_key:
                print("API key cannot be empty. Please try again.")
                continue

            # Clean the API key
            if api_key.startswith("\ufeff"):
                api_key = api_key[1:]

            if not validate_api_key(api_key):
                print("Invalid API key format. API keys should start with 'sk-ant-' and be at least 20 characters.")
                continue

            # Save the key
            try:
                save_api_key_to_config(api_key)
                print("\n✓ API key saved successfully to ~/.clud/anthropic-api-key.key")
                print("You can now use 'clud' to launch Claude-powered development containers.")
                return 0
            except ConfigError as e:
                print(f"\nError: Could not save API key: {e}", file=sys.stderr)
                return 1

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            return 2


def prompt_for_api_key() -> str:
    """Interactively prompt user for API key."""
    print("No Claude API key found.")

    while True:
        try:
            # Flush output to ensure prompt is displayed before input
            sys.stdout.flush()
            api_key = input("Please enter your Anthropic API key: ").strip()
            if not api_key:
                print("API key cannot be empty. Please try again.")
                continue

            if not validate_api_key(api_key):
                print("Invalid API key format. API keys should start with 'sk-ant-' and be at least 20 characters.")
                continue

            # Ask if user wants to save to config
            sys.stdout.flush()
            save_choice = input("Save this key to ~/.clud/ for future use? (y/N): ").strip().lower()
            if save_choice in ["y", "yes"]:
                try:
                    save_api_key_to_config(api_key)
                    print("API key saved to ~/.clud/anthropic-api-key.key")
                except ConfigError as e:
                    print(f"Warning: Could not save API key: {e}")

            return api_key

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def get_api_key(args: Any) -> str:
    """Get API key following priority order: --api-key, --api-key-from, env var, saved config, prompt."""
    api_key = None

    # Priority 0: --api-key command line argument
    if hasattr(args, "api_key") and args.api_key:
        api_key = args.api_key.strip()

    # Priority 1: --api-key-from keyring entry (if keyring is available)
    if not api_key and hasattr(args, "api_key_from") and args.api_key_from:
        with contextlib.suppress(ConfigError):
            api_key = get_api_key_from_keyring(args.api_key_from) if keyring is not None else load_api_key_from_config(args.api_key_from)

    # Priority 2: Environment variable
    if not api_key:
        env_key = os.environ.get("ANTHROPIC_API_KEY")
        if env_key:
            api_key = env_key.strip()

    # Priority 3: Saved config file
    if not api_key:
        api_key = load_api_key_from_config()

    # Priority 4: Interactive prompt
    if not api_key:
        api_key = prompt_for_api_key()

    # Clean the API key before validation
    if api_key:
        api_key = api_key.strip()
        # Remove any BOM characters
        if api_key.startswith("\ufeff"):
            api_key = api_key[1:]

    # Validate the final API key
    if not validate_api_key(api_key):
        raise ValidationError("Invalid API key format")

    # Type checker note: validate_api_key ensures api_key is not None
    assert api_key is not None
    return api_key


# ============================================================================
# Telegram Credential Management
# ============================================================================


def save_telegram_credentials(bot_token: str, chat_id: str) -> None:
    """Save Telegram credentials using the credential store.

    Args:
        bot_token: Telegram bot token (required)
        chat_id: Telegram chat ID (can be empty string if not yet known)
    """
    if keyring is None:
        raise ConfigError("No credential storage available. Install with: pip install keyring, keyrings.cryptfile, or cryptography")

    try:
        # Save bot token (always required)
        keyring.set_password("clud-telegram", "bot-token", bot_token.strip())

        # Save chat_id only if it's not empty
        if chat_id and chat_id.strip():
            keyring.set_password("clud-telegram", "chat-id", chat_id.strip())
    except Exception as e:
        raise ConfigError(f"Failed to save Telegram credentials: {e}") from e


def load_telegram_credentials() -> tuple[str | None, str | None]:
    """Load Telegram credentials from credential store.

    Returns:
        Tuple of (bot_token, chat_id) or (None, None) if not found
    """
    if keyring is None:
        return None, None

    try:
        bot_token = keyring.get_password("clud-telegram", "bot-token")
        chat_id = keyring.get_password("clud-telegram", "chat-id")
        return bot_token, chat_id
    except Exception as e:
        print(f"Warning: Could not load Telegram credentials: {e}", file=sys.stderr)
        return None, None


# ============================================================================
# Claude Executable Discovery
# ============================================================================


def _find_claude_path() -> str | None:
    """Find the path to the Claude executable."""
    # Try to find claude in PATH, prioritizing Windows executables
    if platform.system() == "Windows":
        # On Windows, prefer .cmd and .exe extensions
        claude_path = shutil.which("claude.cmd") or shutil.which("claude.exe")
        if claude_path:
            return claude_path

    # Fall back to generic "claude" (for Unix or git bash on Windows)
    claude_path = shutil.which("claude")
    if claude_path:
        return claude_path

    # Check common Windows npm global locations
    if platform.system() == "Windows":
        possible_paths = [
            os.path.expanduser("~/AppData/Roaming/npm/claude.cmd"),
            os.path.expanduser("~/AppData/Roaming/npm/claude.exe"),
            "C:/Users/" + os.environ.get("USERNAME", "") + "/AppData/Roaming/npm/claude.cmd",
        ]
        for path in possible_paths:
            if os.path.exists(path):
                return path

    return None


# ============================================================================
# Special Command Handlers
# ============================================================================


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
    def open_browser_delayed() -> None:
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


# ============================================================================
# Command Building and Execution
# ============================================================================


def _inject_completion_prompt(message: str, iteration: int | None = None, total_iterations: int | None = None) -> str:
    """Inject the DONE.md completion prompt into the user's message.

    Args:
        message: The user's original message
        iteration: Current iteration number (1-indexed) if in loop mode
        total_iterations: Total number of iterations if in loop mode
    """
    if iteration is not None and total_iterations is not None:
        # Loop mode: build prompt parts conditionally
        parts = [" IMPORTANT:"]

        # Add iteration-specific intro
        if iteration == 1:
            parts.append(f"You are the first agent spawned for this task (iteration 1 of {total_iterations}).")
        else:
            parts.append(f"This is iteration {iteration} of {total_iterations}.")

        # Add common instructions (same for all iterations)
        parts.append(f"Before finishing this iteration, create a summary file named .agent_task/ITERATION_{iteration}.md documenting what you accomplished.")
        parts.append("If you determine that ALL work across ALL iterations is 100% complete, also write .agent_task/DONE.md to halt the loop early.")

        injection = " ".join(parts)
    else:
        # Non-loop mode: standard completion prompt
        injection = " If you see that the task is 100 percent complete, then write out .agent_task/DONE.md and halt"

    return message + injection


def _build_claude_command(
    args: Args,
    claude_path: str,
    inject_prompt: bool = False,
    iteration: int | None = None,
    total_iterations: int | None = None,
) -> list[str]:
    """Build the Claude command with all arguments.

    Args:
        args: Parsed command-line arguments
        claude_path: Path to Claude executable
        inject_prompt: Whether to inject completion prompt
        iteration: Current iteration number (1-indexed) if in loop mode
        total_iterations: Total number of iterations if in loop mode
    """
    cmd = [claude_path, "--dangerously-skip-permissions"]

    if args.continue_flag:
        cmd.append("--continue")

    if args.prompt:
        prompt_text = args.prompt
        if inject_prompt:
            prompt_text = _inject_completion_prompt(prompt_text, iteration, total_iterations)
        cmd.extend(["-p", prompt_text])
        # Enable streaming JSON output for -p flag by default
        # Note: stream-json requires --verbose when used with --print/-p
        cmd.extend(["--output-format", "stream-json", "--verbose"])

    if args.message:
        message_text = args.message
        if inject_prompt:
            message_text = _inject_completion_prompt(message_text, iteration, total_iterations)

        # If idle timeout is set, force non-interactive mode with -p
        # to avoid TUI redraws being detected as activity
        if args.idle_timeout is not None:
            cmd.extend(["-p", message_text])
        else:
            cmd.append(message_text)

    if args.claude_args:
        cmd.extend(args.claude_args)

    return cmd


def _print_debug_info(claude_path: str | None, cmd: list[str], verbose: bool = False) -> None:
    """Print debug information about Claude execution."""
    if not verbose:
        return

    if claude_path:
        print(f"DEBUG: Found claude at: {claude_path}", file=sys.stderr)
        print(f"DEBUG: Platform: {platform.system()}", file=sys.stderr)
        print(f"DEBUG: File exists: {os.path.exists(claude_path)}", file=sys.stderr)

    if cmd:
        print(f"DEBUG: Executing command: {cmd}", file=sys.stderr)


def _print_error_diagnostics(claude_path: str | None, cmd: list[str]) -> None:
    """Print comprehensive error diagnostics."""
    print(f"DEBUG: Current working directory: {os.getcwd()}", file=sys.stderr)
    print(f"DEBUG: Command attempted: {subprocess.list2cmdline(cmd) if cmd else 'command not yet built'}", file=sys.stderr)
    print(f"DEBUG: Claude path used: {claude_path if claude_path else 'path not yet determined'}", file=sys.stderr)
    print("DEBUG: Claude search results:", file=sys.stderr)

    if platform.system() == "Windows":
        print(f"  - shutil.which('claude.cmd'): {shutil.which('claude.cmd')}", file=sys.stderr)
        print(f"  - shutil.which('claude.exe'): {shutil.which('claude.exe')}", file=sys.stderr)

    print(f"  - shutil.which('claude'): {shutil.which('claude')}", file=sys.stderr)
    print(f"  - ~/AppData/Roaming/npm/claude.cmd exists: {os.path.exists(os.path.expanduser('~/AppData/Roaming/npm/claude.cmd'))}", file=sys.stderr)
    print(f"  - ~/AppData/Roaming/npm/claude.exe exists: {os.path.exists(os.path.expanduser('~/AppData/Roaming/npm/claude.exe'))}", file=sys.stderr)


def _execute_command(cmd: list[str], use_shell: bool = False, verbose: bool = False) -> int:
    """Execute a command and return its exit code."""
    if use_shell:
        # Convert command list to shell string and execute through shell
        cmd_str = subprocess.list2cmdline(cmd)
        if verbose:
            print(f"DEBUG: Retrying with shell=True: {cmd_str}", file=sys.stderr)
        result = subprocess.run(cmd_str, shell=True)
    else:
        result = subprocess.run(cmd)

    return result.returncode


# ============================================================================
# Loop Mode Implementation
# ============================================================================


def _prompt_for_loop_count() -> int:
    """Prompt user for loop count."""
    while True:
        try:
            sys.stdout.flush()
            response = input("Loop count: ").strip()
            if not response:
                print("Loop count cannot be empty. Please enter a positive number.")
                continue

            count = int(response)
            if count <= 0:
                print("Loop count must be greater than 0.")
                continue

            return count

        except ValueError:
            print("Invalid input. Please enter a valid number.")
            continue
        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def _prompt_for_message() -> str:
    """Prompt user for agent message/prompt."""
    while True:
        try:
            sys.stdout.flush()
            response = input("Prompt for agent: ").strip()
            if not response:
                print("Prompt cannot be empty. Please try again.")
                continue

            return response

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            sys.exit(2)


def _open_file_in_editor(file_path: Path) -> None:
    """Open a file in the default text editor (cross-platform)."""
    try:
        system = platform.system()
        if system == "Darwin":  # macOS
            subprocess.run(["open", str(file_path)], check=False)
        elif system == "Windows":
            # Try sublime first, then fall back to default
            sublime_paths = [
                "C:\\Program Files\\Sublime Text\\sublime_text.exe",
                "C:\\Program Files\\Sublime Text 3\\sublime_text.exe",
                os.path.expanduser("~\\AppData\\Local\\Programs\\Sublime Text\\sublime_text.exe"),
            ]
            sublime_found = False
            for sublime_path in sublime_paths:
                if os.path.exists(sublime_path):
                    subprocess.run([sublime_path, str(file_path)], check=False)
                    sublime_found = True
                    break

            if not sublime_found:
                # Fall back to default Windows opener
                os.startfile(str(file_path))  # type: ignore
        else:  # Linux and other Unix-like systems
            # Try common editors in order
            editors = ["sublime_text", "subl", "xdg-open"]
            for editor in editors:
                if shutil.which(editor):
                    subprocess.run([editor, str(file_path)], check=False)
                    break
    except Exception as e:
        print(f"Warning: Could not open {file_path}: {e}", file=sys.stderr)


def _handle_existing_agent_task(agent_task_dir: Path) -> tuple[bool, int]:
    """Handle existing .agent_task directory from previous session.

    Args:
        agent_task_dir: Path to .agent_task directory

    Returns:
        Tuple of (should_continue, start_iteration)
        - should_continue: False if user cancelled
        - start_iteration: Iteration number to start from (1 for fresh, N+1 for continuation)
    """
    if not agent_task_dir.exists():
        return True, 1

    # Scan for existing files
    iteration_files = sorted(agent_task_dir.glob("ITERATION_*.md"))
    done_file = agent_task_dir / "DONE.md"

    # If directory is empty, treat as fresh start
    if not iteration_files and not done_file.exists():
        return True, 1

    # Display warning
    print("\n⚠️  Previous agent session detected (.agent_task/ exists)", file=sys.stderr)
    print("Contains:", file=sys.stderr)

    for file in iteration_files:
        mtime = file.stat().st_mtime
        timestamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime))
        print(f"  - {file.name} ({timestamp})", file=sys.stderr)

    if done_file.exists():
        mtime = done_file.stat().st_mtime
        timestamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime))
        print(f"  - DONE.md ({timestamp}) ⚠️  Will halt immediately!", file=sys.stderr)

    # Prompt user
    print(file=sys.stderr)
    sys.stdout.flush()

    try:
        response = input("Delete and start over? [y/n]: ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        print("\nOperation cancelled.", file=sys.stderr)
        return False, 1

    if response in ["y", "yes"]:
        # Delete entire directory
        try:
            shutil.rmtree(agent_task_dir)
            print("✓ Previous session deleted", file=sys.stderr)
            return True, 1
        except Exception as e:
            print(f"Error: Failed to delete .agent_task directory: {e}", file=sys.stderr)
            return False, 1

    elif response in ["n", "no"]:
        # Keep directory, determine next iteration
        last_iteration = 0
        for file in iteration_files:
            # Extract number from ITERATION_N.md
            match = re.match(r"ITERATION_(\d+)\.md", file.name)
            if match:
                last_iteration = max(last_iteration, int(match.group(1)))

        # Remove DONE.md to prevent immediate halt
        if done_file.exists():
            try:
                done_file.unlink()
                print("✓ Removed DONE.md to allow continuation", file=sys.stderr)
            except Exception as e:
                print(f"Warning: Could not remove DONE.md: {e}", file=sys.stderr)

        next_iteration = last_iteration + 1
        print(f"✓ Continuing from iteration {next_iteration}", file=sys.stderr)
        return True, next_iteration

    else:
        print("Invalid response. Operation cancelled.", file=sys.stderr)
        return False, 1


def _run_loop(args: Args, claude_path: str, loop_count: int) -> int:
    """Run Claude in a loop, checking for DONE.md after each iteration."""
    agent_task_dir = Path(".agent_task")

    # Handle existing session from previous run
    should_continue, start_iteration = _handle_existing_agent_task(agent_task_dir)
    if not should_continue:
        return 2  # User cancelled

    # Create .agent_task directory if it doesn't exist (may have been deleted)
    agent_task_dir.mkdir(exist_ok=True)

    done_file = agent_task_dir / "DONE.md"

    # Start from determined iteration (may be > 1 if continuing previous session)
    for i in range(start_iteration - 1, loop_count):
        iteration_num = i + 1
        print(f"\n--- Iteration {iteration_num}/{loop_count} ---", file=sys.stderr)

        # Print the user's prompt for this iteration
        user_prompt = args.prompt if args.prompt else args.message
        if user_prompt:
            print(f"Prompt: {user_prompt}", file=sys.stderr)
            print(file=sys.stderr)  # Empty line for spacing

        # Build command with prompt injection, including iteration context
        cmd = _build_claude_command(
            args,
            claude_path,
            inject_prompt=True,
            iteration=iteration_num,
            total_iterations=loop_count,
        )

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Execute the command with streaming if prompt is present
        if args.prompt:
            # Create JSON formatter for beautiful output in loop mode
            formatter = StreamJsonFormatter(
                show_system=args.verbose,
                show_usage=True,
                show_cache=args.verbose,
                verbose=args.verbose,
            )
            stdout_callback = create_formatter_callback(formatter)
            returncode = RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback)
        else:
            returncode = _execute_command(cmd, use_shell=False, verbose=args.verbose)

        if returncode != 0 and args.verbose:
            print(f"Warning: Iteration {iteration_num} exited with code {returncode}", file=sys.stderr)

        # Check if DONE.md was created
        if done_file.exists():
            print(f"\n✅ .agent_task/DONE.md detected after iteration {iteration_num} — halting early.", file=sys.stderr)
            break

    print("\nAll iterations complete or halted early.", file=sys.stderr)

    # Open DONE.md if it exists
    if done_file.exists():
        print(f"Opening {done_file}...", file=sys.stderr)
        _open_file_in_editor(done_file)

    return 0


# ============================================================================
# Main Agent Execution
# ============================================================================


def run_agent(args: Args) -> int:
    """
    Launch Claude Code with dangerous mode (--dangerously-skip-permissions).
    This bypasses all permission prompts for a more streamlined workflow.

    WARNING: This mode removes all safety guardrails. Use with caution.
    """
    # Initialize variables for exception handler access
    claude_path: str | None = None
    cmd: list[str] = []

    # Load telegram credentials from saved config if not already provided
    if args.telegram and (not args.telegram_bot_token or not args.telegram_chat_id):
        saved_token, saved_chat_id = load_telegram_credentials()
        if not args.telegram_bot_token:
            args.telegram_bot_token = saved_token
        if not args.telegram_chat_id:
            args.telegram_chat_id = saved_chat_id

    # Initialize Telegram bot if enabled
    telegram_bot = TelegramBot.from_args(args)

    # Send invitation if telegram bot is available
    if telegram_bot:
        telegram_bot.send_invitation(project_path=Path.cwd(), mode="foreground")

    try:
        # Validate that we have input when running in non-interactive mode
        # Claude Code requires either a prompt (-p), message (-m), stdin input, or loop mode
        has_input = args.prompt or args.message or args.cmd or args.loop_value is not None
        has_stdin = not sys.stdin.isatty()

        if not has_input and not has_stdin:
            print("Error: Input must be provided either through stdin or as a prompt argument", file=sys.stderr)
            print("Usage:", file=sys.stderr)
            print("  clud -p 'your prompt here'       # Run with prompt", file=sys.stderr)
            print("  clud -m 'your message'           # Send message", file=sys.stderr)
            print("  echo 'prompt' | clud             # Pipe input", file=sys.stderr)
            print("  clud --loop 5 -p 'prompt'        # Run in loop mode", file=sys.stderr)
            print("  clud                             # Interactive mode (reads from stdin)", file=sys.stderr)
            return 1

        # If --cmd is provided, execute the command directly instead of launching Claude
        if args.cmd:
            result = subprocess.run(args.cmd, shell=True)
            return result.returncode

        # Handle dry-run mode
        if args.dry_run:
            cmd_parts = ["claude", "--dangerously-skip-permissions"]
            if args.continue_flag:
                cmd_parts.append("--continue")
            if args.prompt:
                cmd_parts.extend(["-p", args.prompt])
                # Enable streaming JSON output for -p flag by default
                # Note: stream-json requires --verbose when used with --print/-p
                cmd_parts.extend(["--output-format", "stream-json", "--verbose"])
            if args.message:
                cmd_parts.append(args.message)
            if args.claude_args:
                cmd_parts.extend(args.claude_args)
            print("Would execute:", " ".join(cmd_parts))
            return 0

        # Find Claude executable
        claude_path = _find_claude_path()
        if not claude_path:
            print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
            print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
            return 1

        # Handle loop mode - parse loop_value flexibly
        if args.loop_value is not None:
            loop_count = None
            loop_message = None

            # Try to parse loop_value
            if args.loop_value == "":
                # --loop with no value: prompt for both
                pass
            else:
                # Try parsing as integer first
                try:
                    loop_count = int(args.loop_value)
                    if loop_count <= 0:
                        print("Error: --loop count must be greater than 0", file=sys.stderr)
                        return 1
                except ValueError:
                    # Not an integer, treat as message
                    loop_message = args.loop_value

            # Prompt for missing values
            # Check if we have a message from loop_value, -m, or -p
            if not args.prompt and not args.message and not loop_message:
                loop_message = _prompt_for_message()

            if loop_count is None:
                loop_count = _prompt_for_loop_count()

            # Set the prompt if we got it from loop_value (uses -p instead of -m)
            if loop_message and not args.message and not args.prompt:
                args.prompt = loop_message

            return _run_loop(args, claude_path, loop_count)

        # Build command
        cmd = _build_claude_command(args, claude_path)

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Execute Claude with the dangerous permissions flag
        # Use idle detection if timeout is specified
        if args.idle_timeout is not None:
            # Create output filter to suppress terminal capability responses
            output_filter = OutputFilter()

            # Output callback to print data to stdout (with filtering)
            def output_callback(data: str) -> None:
                # Filter out terminal capability responses to prevent corrupting parent terminal
                filtered_data = output_filter.filter_terminal_responses(data)
                if filtered_data:
                    sys.stdout.write(filtered_data)
                    sys.stdout.flush()

            detect_agent_completion(cmd, args.idle_timeout, output_callback)
            return 0
        elif args.prompt:
            # Use RunningProcess for streaming output when using -p flag
            # This ensures stream-json output is displayed line-by-line in real-time
            # Create JSON formatter for beautiful output
            formatter = StreamJsonFormatter(
                show_system=args.verbose,
                show_usage=True,
                show_cache=args.verbose,
                verbose=args.verbose,
            )
            stdout_callback = create_formatter_callback(formatter)
            return RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback)
        else:
            return _execute_command(cmd, use_shell=False, verbose=args.verbose)

    except FileNotFoundError as e:
        print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
        print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
        print(f"DEBUG: FileNotFoundError details: {e}", file=sys.stderr)
        traceback.print_exc()
        return 1

    except KeyboardInterrupt:
        print("\nInterrupted by user", file=sys.stderr)
        return 130

    except OSError as e:
        print(f"Error launching Claude: {e}", file=sys.stderr)
        print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
        _print_error_diagnostics(claude_path, cmd)

        # Try backup method with shell=True
        if cmd and claude_path:
            try:
                print("\nAttempting backup method (shell=True)...", file=sys.stderr)
                return _execute_command(cmd, use_shell=True, verbose=args.verbose)
            except Exception as shell_error:
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()

        print("\nFull stack trace from original error:", file=sys.stderr)
        traceback.print_exc()
        return 1

    except Exception as e:
        print(f"Error launching Claude: {e}", file=sys.stderr)
        print(f"DEBUG: Exception type: {type(e).__name__}", file=sys.stderr)
        _print_error_diagnostics(claude_path, cmd)

        # Try backup method with shell=True
        if cmd and claude_path:
            try:
                print("\nAttempting backup method (shell=True)...", file=sys.stderr)
                return _execute_command(cmd, use_shell=True, verbose=args.verbose)
            except Exception as shell_error:
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()

        print("\nFull stack trace from original error:", file=sys.stderr)
        traceback.print_exc()
        return 1

    finally:
        # Send cleanup notification if telegram bot is available
        if telegram_bot:
            telegram_bot.send_cleanup()


def main(args_list: list[str] | None = None) -> int:
    """Main entry point for clud - handles routing and execution."""
    try:
        # Parse arguments
        args = parse_args(args_list)

        # Handle help first
        if args.help:
            print("clud - Claude Code in YOLO mode")
            print("Usage: clud [options...]")
            print()
            print("Special modes:")
            print("  fix [URL]            Fix linting and test issues (with optional GitHub URL)")
            print("  up [-p|--publish]    Run global codeup command with auto-fix")
            print()
            print("Special commands:")
            print("  --login              Configure API key for Claude")
            print("  --task PATH          Open task file in editor")
            print("  --code [PORT]        Launch code-server in browser (default port: 8080)")
            print("  --lint               Run global linting with codeup")
            print("  --test               Run tests with codeup")
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
        if args.login:
            return handle_login()

        if args.task is not None:
            return handle_task_command(args.task)

        if args.lint:
            return handle_lint_command()

        if args.test:
            return handle_test_command()

        if args.kanban:
            return handle_kanban_command()

        if args.telegram_web:
            return handle_telegram_command(args.telegram_token)

        if args.webui:
            return handle_webui_command(args.webui_port)

        if args.code:
            return handle_code_command(args.code_port)

        if args.fix:
            return handle_fix_command(args.fix_url)

        if args.init_loop:
            return handle_init_loop_command()

        # Route to appropriate mode handler
        if args.mode == AgentMode.FIX:
            return handle_fix_command(args.fix_url)
        elif args.mode == AgentMode.UP:
            # Check if publish flag was provided
            if args.up_publish:
                return handle_codeup_publish_command()
            else:
                return handle_codeup_command()
        else:
            # Default mode - run foreground agent
            # If --track is enabled, set up tracking before launching agent
            if args.track:
                from .agent.tracking import create_tracker

                # Get command from args or use default description
                command = args.prompt or args.message or "claude code"
                tracker = create_tracker(command)

                exit_code = 1  # Default exit code in case of exception
                try:
                    exit_code = run_agent(args)
                finally:
                    tracker.stop(exit_code)

                return exit_code
            else:
                return run_agent(args)

    except (ValidationError, ConfigError) as e:
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
