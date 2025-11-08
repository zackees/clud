"""Consolidated agent module - handles all Claude Code execution and special commands."""

import contextlib
import logging
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
import uuid
import webbrowser
from pathlib import Path
from typing import Any

from running_process import RunningProcess

from .agent.completion import detect_agent_completion
from .agent.task_info import TaskInfo
from .agent_args import AgentMode, Args, parse_args
from .claude_installer import (
    find_claude_code,
    find_npm_executable,
    get_claude_version,
    get_clud_bin_dir,
    get_clud_npm_dir,
    get_local_claude_path,
    install_claude_local,
    is_claude_installed_locally,
    prompt_install_claude,
)
from .hooks import HookContext, HookEvent, get_hook_manager
from .hooks.config import load_hook_config
from .hooks.telegram import TelegramHookHandler
from .hooks.webhook import WebhookHookHandler
from .json_formatter import StreamJsonFormatter, create_formatter_callback
from .output_filter import OutputFilter
from .secrets import get_credential_store
from .settings_manager import get_model_preference, set_model_preference
from .shell_config import ShellLaunchConfig
from .task import handle_task_command
from .telegram_bot import TelegramBot
from .util import detect_git_bash
from .util.process import launch_detached

# Get credential store once at module level
keyring = get_credential_store()

# Initialize logger
logger = logging.getLogger(__name__)


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
# Hook System Helpers
# ============================================================================


def register_hooks_from_config(instance_id: str, session_id: str, hook_debug: bool = False) -> None:
    """Register hooks based on configuration file and environment variables.

    Args:
        instance_id: Unique ID for this agent instance
        session_id: Session ID (typically same as instance_id for standalone runs)
        hook_debug: Whether to enable debug logging for hooks
    """
    try:
        # Load hook configuration
        config = load_hook_config()

        if not config.enabled:
            if hook_debug:
                print("DEBUG: Hooks disabled in configuration", file=sys.stderr)
            return

        hook_manager = get_hook_manager()

        # Register Telegram hook if enabled
        if config.telegram_enabled and config.telegram_bot_token and config.telegram_chat_id:
            telegram_handler = TelegramHookHandler(
                bot_token=config.telegram_bot_token,
                buffer_size=config.buffer_size,
                flush_interval=config.flush_interval,
            )
            hook_manager.register(telegram_handler)
            if hook_debug:
                print("DEBUG: Registered Telegram hook (will use session_id as chat_id)", file=sys.stderr)

        # Register webhook hook if enabled
        if config.webhook_enabled and config.webhook_url:
            webhook_handler = WebhookHookHandler(
                webhook_url=config.webhook_url,
                secret=config.webhook_secret,
            )
            hook_manager.register(webhook_handler)
            if hook_debug:
                print(f"DEBUG: Registered webhook hook (url={config.webhook_url})", file=sys.stderr)

    except KeyboardInterrupt:
        raise
    except Exception as e:
        if hook_debug:
            print(f"DEBUG: Failed to register hooks: {e}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
        # Don't fail if hooks can't be registered - hooks are optional


def trigger_hook_sync(event: HookEvent, context: HookContext, hook_debug: bool = False) -> None:
    """Trigger a hook event synchronously.

    Args:
        event: The hook event type
        context: The hook context
        hook_debug: Whether to print debug info
    """
    try:
        hook_manager = get_hook_manager()

        # Skip if no handlers registered
        if not hook_manager.has_handlers(event):
            if hook_debug:
                print(f"DEBUG: No handlers for event {event.value}", file=sys.stderr)
            return

        if hook_debug:
            print(f"DEBUG: Triggering hook event: {event.value}", file=sys.stderr)

        # Trigger synchronously - just pass context (event is inside context)
        hook_manager.trigger_sync(context)

    except KeyboardInterrupt:
        raise
    except Exception as e:
        if hook_debug:
            print(f"DEBUG: Hook trigger failed: {e}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
        # Don't fail if hooks fail - they are optional


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


def set_terminal_title() -> None:
    """Set terminal title to 'clud: {parent_dir}' where parent_dir is the parent directory of cwd."""
    try:
        # Only set terminal title if stdout is a TTY (not redirected/captured)
        if not sys.stdout.isatty():
            return

        cwd = Path.cwd()
        # Get parent directory name (the directory containing the current directory)
        parent_dir = cwd.parent.name if cwd.parent.name else cwd.name

        # Use ANSI escape sequence to set terminal title
        # \033]0; sets the title, \007 (bell) terminates it
        # This works on Windows (Git Bash, Windows Terminal), macOS, and Linux
        title = f"clud: {parent_dir}"
        sys.stdout.write(f"\033]0;{title}\007")
        sys.stdout.flush()
    except (OSError, AttributeError) as e:
        # Terminal title is nice-to-have, not critical - log at debug level
        logger.debug("Failed to set terminal title: %s", e)


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
            except (OSError, AttributeError) as e:
                # Not critical if hiding fails - log at debug level
                logger.debug("Failed to set file as hidden on Windows: %s", e)

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
    """Find the path to the Claude executable.

    This is now a wrapper around claude_installer.find_claude_code()
    for backward compatibility.
    """
    return find_claude_code()


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
    """Handle the --lint command by running clud with a message to run lint-test."""
    lint_prompt = "run lint-test, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"
    return run_clud_subprocess(lint_prompt)


def handle_test_command() -> int:
    """Handle the --test command by running clud with a message to run lint-test."""
    test_prompt = "run lint-test, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"
    return run_clud_subprocess(test_prompt)


def _check_agent_artifacts() -> bool:
    """Check for agent task artifacts before running clud up.

    Checks for DONE.md, LOOP.md, and .agent_task/ directory.
    Prompts user to delete or abort if found.

    Returns:
        True if should continue, False if should abort
    """
    artifacts = ["DONE.md", "LOOP.md", ".agent_task"]
    found_artifacts: list[str] = []
    tracked_and_modified_artifacts: list[str] = []

    # Check which artifacts exist and are new or modified
    for artifact in artifacts:
        artifact_path = Path(artifact)
        if not artifact_path.exists():
            continue

        # Check git status to determine if artifact is new or modified
        try:
            # Check if file is tracked (exists in previous commits)
            ls_result = subprocess.run(["git", "ls-files", artifact], capture_output=True, text=True, check=False)
            is_tracked = ls_result.returncode == 0 and ls_result.stdout.strip()

            if is_tracked:
                # Check if modified (using git status --porcelain)
                status_result = subprocess.run(["git", "status", "--porcelain", artifact], capture_output=True, text=True, check=False)
                is_modified = status_result.returncode == 0 and status_result.stdout.strip()

                if is_modified:
                    # Tracked and modified - include in prompt with warning
                    found_artifacts.append(artifact)
                    tracked_and_modified_artifacts.append(artifact)
                # If tracked but NOT modified (unchanged), skip it - no prompt needed
            else:
                # Not tracked (new file) - include in prompt
                found_artifacts.append(artifact)
        except Exception:
            # Git not available or not a git repo - treat as new file
            found_artifacts.append(artifact)

    if not found_artifacts:
        return True  # No artifacts, continue

    # ANSI color codes
    YELLOW = "\033[93m"
    RED = "\033[91m"
    BOLD = "\033[1m"
    RESET = "\033[0m"

    # Display found artifacts
    print(f"\n{RED}{BOLD}Found {len(found_artifacts)} artifact file(s) from an agent loop{RESET}")
    for artifact in found_artifacts:
        print(f"  * {artifact}")
    print()

    # Warn about tracked and modified artifacts
    if tracked_and_modified_artifacts:
        print(f"{YELLOW}Warning: The following artifacts were found in previous commits but have been modified:{RESET}")
        for artifact in tracked_and_modified_artifacts:
            print(f"  - {artifact}")
        print(f"{YELLOW}It's recommended you should purge these files{RESET}")
        print()

    # Prompt user
    while True:
        try:
            sys.stdout.flush()
            response = input("Do you want to [a]bort or [d]elete and continue? ").strip().lower()

            if response in ["a", "abort"]:
                print(f"\n{RED}Aborted: Agent loop artifacts need to be cleared before 'clud up' can run.{RESET}")
                return False

            elif response in ["d", "delete"]:
                # Delete artifacts
                for artifact in found_artifacts:
                    artifact_path = Path(artifact)
                    try:
                        if artifact_path.is_dir():
                            shutil.rmtree(artifact_path)
                            print(f"Deleted directory: {artifact}")
                        else:
                            artifact_path.unlink()
                            print(f"Deleted file: {artifact}")
                    except Exception as e:
                        print(f"Error deleting {artifact}: {e}", file=sys.stderr)
                        return False

                print("✓ Artifacts deleted. Continuing with 'clud up'...\n")
                return True

            else:
                print("Invalid choice. Please enter 'a' to abort or 'd' to delete.")
                continue

        except (EOFError, KeyboardInterrupt):
            print("\nOperation cancelled.")
            return False


def handle_codeup_command() -> int:
    """Handle the --codeup command by running git pre-check first, then clud with a message to run the global codeup command."""
    # Check for agent task artifacts first
    if not _check_agent_artifacts():
        return 1  # User aborted

    # Run git pre-check using CodeUp API
    from .git_precheck import run_two_phase_precheck

    print("Running git pre-check before agent invocation...", file=sys.stderr)
    try:
        result = run_two_phase_precheck(verbose=True)
        if not result.success:
            _print_red_banner("GIT PRE-CHECK FAILED")
            print(f"Error: {result.error_message}", file=sys.stderr)
            return 1
    except Exception as e:
        print(f"Warning: Error running git pre-check: {e}", file=sys.stderr)

    # Now run the agent with the codeup prompt
    codeup_prompt = (
        "run the global command codeup normally through the shell (it's a global command installed on the system), "
        "if it returns 0, halt, if it fails then read the output logs and apply the fixes. "
        "Run upto 5 times before giving up, else halt."
    )
    return run_clud_subprocess(codeup_prompt)


def handle_codeup_publish_command() -> int:
    """Handle the --codeup-publish command by running git pre-check first, then clud with a message to run codeup -p."""
    # Check for agent task artifacts first
    if not _check_agent_artifacts():
        return 1  # User aborted

    # Run git pre-check using CodeUp API
    from .git_precheck import run_two_phase_precheck

    print("Running git pre-check before agent invocation...", file=sys.stderr)
    try:
        result = run_two_phase_precheck(verbose=True)
        if not result.success:
            _print_red_banner("GIT PRE-CHECK FAILED")
            print(f"Error: {result.error_message}", file=sys.stderr)
            return 1
    except Exception as e:
        print(f"Warning: Error running git pre-check: {e}", file=sys.stderr)

    # Now run the agent with the codeup -p prompt
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
    """Handle the --telegram/-tg command by launching Telegram integration server via daemon.

    Automatically starts the daemon-based Telegram server if credentials are available.
    Falls back to landing page if no credentials found.

    Args:
        token: Optional bot token to save

    Returns:
        Exit code
    """
    try:
        # Save token if provided
        if token:
            print("Saving Telegram bot token...")
            try:
                save_telegram_credentials(token, "")
                print("✓ Token saved successfully\n")
            except Exception as e:
                print(f"Warning: Could not save token: {e}\n", file=sys.stderr)

        # Load credentials from environment or saved config
        saved_token, _ = load_telegram_credentials()
        env_token = os.environ.get("TELEGRAM_BOT_TOKEN")

        # Prioritize env vars, fall back to saved
        bot_token = env_token or saved_token or token

        # If we have a bot token, launch Telegram server via daemon
        if bot_token:
            print("✅ Telegram bot token found")
            print(f"Bot Token: {bot_token[:20]}...")
            print()
            # Launch the full Telegram integration server via daemon
            return handle_telegram_server_command(port=None, config_path=None)

        # Otherwise, launch landing page mode
        print("⚠️  No Telegram bot token found")
        print("Please provide a bot token:")
        print("  1. Set TELEGRAM_BOT_TOKEN environment variable, OR")
        print("  2. Run: clud --telegram YOUR_BOT_TOKEN")
        print()
        print("To get a bot token, message @BotFather on Telegram")
        print()
        print("Launching landing page...")
        print()

        from .webapp.server import run_server

        return run_server()

    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        return 1


def handle_telegram_server_command(port: int | None = None, config_path: str | None = None) -> int:
    """Handle the --telegram-server command by ensuring telegram service via daemon.

    Args:
        port: Optional port for web interface (default: 8889)
        config_path: Optional path to configuration file

    Returns:
        Exit code
    """
    try:
        import webbrowser

        from .service import ensure_telegram_running

        print("Starting Telegram Integration Server via daemon...")
        print()

        # Ensure telegram service is running via daemon
        if not ensure_telegram_running(config_path=config_path, port=port):
            print("ERROR: Failed to start telegram service", file=sys.stderr)
            print("Check logs for details:", file=sys.stderr)
            print("  - Daemon logs: ~/.config/clud/daemon.log (if logging enabled)", file=sys.stderr)
            print("  - Telegram config: Use --telegram-config to specify config file", file=sys.stderr)
            print("  - Bot token: Set TELEGRAM_BOT_TOKEN environment variable", file=sys.stderr)
            return 1

        # Get status to display info
        import json
        import urllib.request

        from .service.server import DAEMON_HOST, DAEMON_PORT

        status_url = f"http://{DAEMON_HOST}:{DAEMON_PORT}/telegram/status"
        try:
            with urllib.request.urlopen(status_url, timeout=2.0) as response:
                status = json.loads(response.read().decode("utf-8"))

                print("✓ Telegram service is running")
                print()
                print("Configuration:")
                print(f"  Bot Token: {'✓ Configured' if status.get('bot_configured') else '✗ Missing'}")
                print(f"  Web URL: http://{status.get('host', '127.0.0.1')}:{status.get('port', 8889)}")
                print(f"  Daemon Port: {DAEMON_PORT}")
                print()

                # Open browser
                web_url = f"http://{status.get('host', '127.0.0.1')}:{status.get('port', 8889)}"
                print(f"Opening browser to {web_url}...")
                webbrowser.open(web_url)
                print()
                print("Service is running in background via daemon (port 7565)")
                print("Use 'clud --telegram-server' again to check status")
                print("To stop: Contact daemon or restart system")

        except Exception as e:
            print(f"Warning: Could not retrieve status: {e}", file=sys.stderr)
            print("Service may be starting... check http://127.0.0.1:8889", file=sys.stderr)

        return 0

    except ImportError as e:
        print(f"Error: Missing required dependency: {e}", file=sys.stderr)
        print("Install with: pip install python-telegram-bot pyyaml", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        import traceback

        traceback.print_exc()
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


def handle_api_server_command(port: int | None = None) -> int:
    """Handle the --api-server command by launching the Message Handler API server."""
    try:
        # Set default port if not provided
        if port is None:
            port = 8765

        print(f"Starting Message Handler API server on port {port}...")
        print(f"API will be available at http://localhost:{port}")
        print()
        print("Endpoints:")
        print(f"  - POST   http://localhost:{port}/api/message")
        print(f"  - GET    http://localhost:{port}/api/instances")
        print(f"  - GET    http://localhost:{port}/api/instances/{{id}}")
        print(f"  - DELETE http://localhost:{port}/api/instances/{{id}}")
        print(f"  - POST   http://localhost:{port}/api/cleanup")
        print(f"  - GET    http://localhost:{port}/health")
        print()
        print("Press Ctrl+C to stop the server")
        print()

        # Import uvicorn and run the server
        import uvicorn

        from clud.api.server import create_app

        app = create_app()
        uvicorn.run(app, host="127.0.0.1", port=port, log_level="info")
        return 0

    except ImportError as e:
        print(f"Error: Missing required dependency: {e}", file=sys.stderr)
        print("Install with: pip install fastapi uvicorn", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\n\nStopping API server...", file=sys.stderr)
        return 0
    except Exception as e:
        print(f"Error running API server: {e}", file=sys.stderr)
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


def handle_info_command() -> int:
    """Handle the --info command by displaying Claude Code installation information."""
    print("Claude Code Installation Information", file=sys.stderr)
    print("=" * 70, file=sys.stderr)
    print(file=sys.stderr)

    # Find Claude Code executable
    claude_path = find_claude_code()

    if not claude_path:
        print("Status: NOT FOUND", file=sys.stderr)
        print(file=sys.stderr)
        print("Claude Code is not installed or not in PATH.", file=sys.stderr)
        print("Install with: clud --install-claude", file=sys.stderr)
        return 1

    # Display installation path
    print("Status: INSTALLED", file=sys.stderr)
    print(file=sys.stderr)
    print("Executable Path:", file=sys.stderr)
    print(f"  {claude_path}", file=sys.stderr)
    print(file=sys.stderr)

    # Get and display version
    version = get_claude_version(claude_path)
    if version:
        print("Version:", file=sys.stderr)
        print(f"  {version}", file=sys.stderr)
    else:
        print("Version: Unable to determine", file=sys.stderr)
    print(file=sys.stderr)

    # Display installation type
    local_path = get_local_claude_path()
    if local_path and str(local_path) == claude_path:
        print("Installation Type: Local (clud-managed)", file=sys.stderr)
        print(f"  Installed in: {get_clud_npm_dir()}", file=sys.stderr)
    else:
        print("Installation Type: System/Global", file=sys.stderr)
        print("  Found in PATH", file=sys.stderr)
    print(file=sys.stderr)

    # Display npm information
    npm_path = find_npm_executable()
    if npm_path:
        print("npm Executable:", file=sys.stderr)
        print(f"  {npm_path}", file=sys.stderr)
    else:
        print("npm Executable: NOT FOUND", file=sys.stderr)
    print(file=sys.stderr)

    # Display clud directories
    print("clud Directories:", file=sys.stderr)
    print(f"  npm packages: {get_clud_npm_dir()}", file=sys.stderr)
    print(f"  binaries: {get_clud_bin_dir()}", file=sys.stderr)
    print(file=sys.stderr)

    # Display platform information
    print("Platform Information:", file=sys.stderr)
    print(f"  OS: {platform.system()}", file=sys.stderr)
    print(f"  Python: {sys.version.split()[0]}", file=sys.stderr)
    print(file=sys.stderr)

    print("=" * 70, file=sys.stderr)
    return 0


def handle_install_claude_command() -> int:
    """Handle the --install-claude command by installing Claude Code locally."""
    print("Installing Claude Code to ~/.clud/npm...", file=sys.stderr)
    print(file=sys.stderr)

    # Check if already installed
    if is_claude_installed_locally():
        print("Claude Code is already installed locally.", file=sys.stderr)
        claude_path = find_claude_code()
        if claude_path:
            print(f"Location: {claude_path}", file=sys.stderr)

        sys.stdout.flush()
        response = input("\nReinstall? [y/N]: ").strip().lower()
        if response not in ["y", "yes"]:
            print("Installation cancelled.", file=sys.stderr)
            return 0

    # Install Claude Code
    if install_claude_local(verbose=True):
        print(file=sys.stderr)
        print("✓ Installation complete!", file=sys.stderr)
        print("You can now use 'clud' to run Claude Code.", file=sys.stderr)
        return 0
    else:
        print(file=sys.stderr)
        print("✗ Installation failed.", file=sys.stderr)
        print(file=sys.stderr)
        print("You can try installing globally instead:", file=sys.stderr)
        print("  npm install -g @anthropic-ai/claude-code@latest", file=sys.stderr)
        return 1


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
    base_fix_instructions = "run `lint-test` upto 5 times, fixing on each time or until it passes. If you run into a locked file then try two times, same with misc system error. Else halt."

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
        fix_prompt = "run `lint-test` upto 5 times, fixing on each time or until it passes. If you run into a locked file then try two times, same with misc system error. Else halt."
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
        parts.append("If you determine that ALL work across ALL iterations is 100% complete, also write DONE.md at the PROJECT ROOT (not .agent_task/) to halt the loop early.")
        parts.append("CRITICAL: NEVER delete or overwrite an existing DONE.md file - it is the terminal signal to halt the loop.")
        parts.append(
            "CRITICAL: YOU CAN NEVER ASK QUESTIONS AND EXPECT ANSWERS! THIS IS AN AGENT LOOP. NO QUESTIONS TO THE USER! "
            "If you must ask a question, then leave it for the next iteration to research or resolve."
        )
        parts.append("If DONE.md already exists, read it first to understand the completion status before proceeding.")
        parts.append("IMPORTANT: Maximize parallel execution - run as many independent operations in parallel as possible to improve efficiency.")

        injection = " ".join(parts)
    else:
        # Non-loop mode: standard completion prompt (also using project root)
        injection = (
            " If you see that the task is 100 percent complete, then write out DONE.md at the project root and halt. "
            "IMPORTANT: Maximize parallel execution - run as many independent operations in parallel as possible to improve efficiency."
        )

    return message + injection


def _get_model_from_args(claude_args: list[str] | None) -> str | None:
    """Detect which model is being used from claude_args or saved settings.

    Returns the model flag (e.g., '--haiku', '--sonnet') or None if not specified.
    Checks args first, then falls back to saved preferences.
    """
    # Check for common model flags in provided arguments
    if claude_args:
        model_flags = ["--haiku", "--sonnet", "--opus", "--claude-3-5-sonnet", "--claude-3-opus"]
        for flag in model_flags:
            if flag in claude_args:
                return flag

    # Fall back to saved preference
    saved_model = get_model_preference()
    return saved_model


def _print_model_message(model_flag: str | None) -> None:
    """Print a message about which model is being loaded."""
    if model_flag == "--haiku":
        print("Loading Haiku 4.5...", file=sys.stderr)
    elif model_flag == "--sonnet":
        print("Loading Sonnet 4.5...", file=sys.stderr)
    else:
        # For any other model, extract a readable name
        if model_flag:
            # Remove dashes and capitalize for display
            display_name = model_flag.lstrip("-").replace("-", " ").title()
            print(f"Loading {display_name}...", file=sys.stderr)


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
        # Enable streaming JSON output for -p flag by default (unless --plain is used)
        # Note: stream-json requires --verbose when used with --print/-p
        if not args.plain:
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

    # Note: Claude Code CLI doesn't support --haiku, --sonnet, etc. flags
    # These custom flags are only used for internal messaging and preferences
    # Don't append model flags to the actual Claude command

    # If a model was explicitly provided in args, save it as the preference
    if args.claude_args:
        for arg in args.claude_args:
            if arg in ["--haiku", "--sonnet", "--opus", "--claude-3-5-sonnet", "--claude-3-opus"]:
                set_model_preference(arg)
                break

    if args.claude_args:
        cmd.extend(args.claude_args)

    return cmd


def _wrap_command_for_git_bash(cmd: list[str]) -> list[str]:
    """Wrap command in git-bash on Windows if available.

    On Windows, if git-bash is detected, this wraps the command to execute
    through git-bash rather than cmd.exe. This provides a proper bash environment
    for Claude Code, avoiding WSL and ensuring consistent behavior.

    Args:
        cmd: Original command as list of strings

    Returns:
        Modified command wrapped in git-bash if on Windows and git-bash is available,
        otherwise returns the original command unchanged.
    """
    if platform.system() != "Windows":
        # Not Windows, return unchanged
        return cmd

    git_bash = detect_git_bash()
    if not git_bash:
        # No git-bash available, return unchanged
        return cmd

    # Convert command to bash-compatible format
    # 1. Convert Windows paths (backslashes) to forward slashes for bash
    # 2. Use bash-style single quoting to avoid escaping issues
    bash_cmd_parts: list[str] = []
    for arg in cmd:
        # Convert Windows paths to forward slashes (bash on Windows understands these)
        if "\\" in arg:
            arg = arg.replace("\\", "/")

        # Use single quotes for bash (no variable expansion, simpler escaping)
        # Escape any single quotes in the argument
        arg_escaped = arg.replace("'", "'\\''")
        bash_cmd_parts.append(f"'{arg_escaped}'")

    cmd_str = " ".join(bash_cmd_parts)

    # Wrap in git-bash: bash -c "command"
    # Use -c flag to execute command directly
    return [git_bash, "-c", cmd_str]


def _build_shell_command(claude_path: str, command_args: list[str]) -> list[str]:
    """Build command using ShellLaunchConfig for proper shell and path handling.

    This function:
    1. Preserves the original claude_path without premature normalization
    2. Determines the best shell to use (prefers git-bash on Windows)
    3. Tests that the shell can be launched
    4. Normalizes the path at the last moment before execution

    Args:
        claude_path: Original path to Claude executable
        command_args: Command-line arguments (e.g., ["--dangerously-skip-permissions", "-p", "message"])

    Returns:
        Final command list ready for subprocess.run(), with path normalized for the target shell
    """
    config = ShellLaunchConfig(
        claude_path=claude_path,
        command_args=command_args,
    )
    return config.build_command()


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
    """Prompt user for loop count (default: 50)."""
    while True:
        try:
            sys.stdout.flush()
            response = input("Loop count [50]: ").strip()
            if not response:
                return 50  # Default to 50

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
    """Open a file in the default text editor (cross-platform, non-blocking)."""
    try:
        system = platform.system()
        if system == "Darwin":  # macOS
            # Try Sublime Text first with --new-window, then fall back to system open
            if shutil.which("subl"):
                launch_detached(["subl", "--new-window", str(file_path)])
            else:
                # macOS 'open' command is already non-blocking
                subprocess.run(["open", str(file_path)], check=False)
        elif system == "Windows":
            # Try editors in order: Sublime Text, TextPad, Notepad
            sublime_paths = [
                Path("C:\\Program Files\\Sublime Text\\sublime_text.exe"),
                Path("C:\\Program Files\\Sublime Text 3\\sublime_text.exe"),
                Path(os.path.expanduser("~\\AppData\\Local\\Programs\\Sublime Text\\sublime_text.exe")),
            ]

            # Try Sublime Text with --new-window
            for sublime_path in sublime_paths:
                if sublime_path.exists():
                    launch_detached([str(sublime_path), "--new-window", str(file_path)])
                    return

            # Try 'subl' in PATH
            if shutil.which("subl"):
                launch_detached(["subl", "--new-window", str(file_path)])
                return

            # Try TextPad
            textpad_paths = [
                Path("C:\\Program Files\\TextPad 9\\TextPad.exe"),
                Path("C:\\Program Files\\TextPad 8\\TextPad.exe"),
                Path("C:\\Program Files (x86)\\TextPad 9\\TextPad.exe"),
                Path("C:\\Program Files (x86)\\TextPad 8\\TextPad.exe"),
            ]
            for textpad_path in textpad_paths:
                if textpad_path.exists():
                    launch_detached([str(textpad_path), str(file_path)])
                    return

            # Fall back to notepad (always available, non-blocking via detached process)
            launch_detached(["notepad.exe", str(file_path)])

        else:  # Linux and other Unix-like systems
            # Try editors in order with detached process
            editors = ["subl", "sublime_text", "gedit", "kate", "nano"]
            for editor in editors:
                if shutil.which(editor):
                    if editor in ["subl", "sublime_text"]:
                        launch_detached([editor, "--new-window", str(file_path)])
                    else:
                        launch_detached([editor, str(file_path)])
                    return

            # Final fallback: xdg-open (delegates to system default)
            if shutil.which("xdg-open"):
                # xdg-open is typically non-blocking, but use launch_detached to be safe
                launch_detached(["xdg-open", str(file_path)])

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

    # Check for DONE.md at project root (new location)
    done_file_root = Path("DONE.md")

    # If directory is empty and no root DONE.md, treat as fresh start
    if not iteration_files and not done_file_root.exists():
        return True, 1

    # Display warning
    print("\n⚠️  Previous agent session detected (.agent_task/ exists)", file=sys.stderr)
    print("Contains:", file=sys.stderr)

    for file in iteration_files:
        mtime = file.stat().st_mtime
        timestamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime))
        print(f"  - {file.name} ({timestamp})", file=sys.stderr)

    # Check for DONE.md at project root
    if done_file_root.exists():
        mtime = done_file_root.stat().st_mtime
        timestamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime))
        print(f"\n  - DONE.md at project root ({timestamp}) ⚠️  Will halt immediately!", file=sys.stderr)

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

        next_iteration = last_iteration + 1
        print(f"✓ Continuing from iteration {next_iteration}", file=sys.stderr)
        return True, next_iteration

    else:
        print("Invalid response. Operation cancelled.", file=sys.stderr)
        return False, 1


def _print_red_banner(message: str) -> None:
    """Print a red banner message to stderr for critical warnings."""
    terminal_width = shutil.get_terminal_size((80, 20)).columns
    banner_char = "="
    padding_char = " "

    # Build banner lines
    border: str = banner_char * terminal_width
    padding: str = padding_char * terminal_width

    # Center the message
    message_lines: list[str] = message.split("\n")
    centered_lines: list[str] = []
    for line in message_lines:
        spaces_needed = max(0, (terminal_width - len(line)) // 2)
        centered_line: str = padding_char * spaces_needed + line
        # Pad to full width
        centered_line = centered_line + padding_char * (terminal_width - len(centered_line))
        centered_lines.append(centered_line)

    # ANSI color codes: red background + white text
    RED_BG = "\033[41m"
    WHITE_TEXT = "\033[97m"
    BOLD = "\033[1m"
    RESET = "\033[0m"

    # Print banner
    print(file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{border}{RESET}", file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{padding}{RESET}", file=sys.stderr)
    for centered_line_text in centered_lines:
        print(f"{RED_BG}{WHITE_TEXT}{BOLD}{centered_line_text}{RESET}", file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{padding}{RESET}", file=sys.stderr)
    print(f"{RED_BG}{WHITE_TEXT}{BOLD}{border}{RESET}", file=sys.stderr)
    print(file=sys.stderr)


def _find_and_run_lint_test() -> tuple[int, str]:
    """Find lint-test command using shutil.which and run it with output capture.

    Returns:
        Tuple of (returncode, output)

    Raises:
        FileNotFoundError: If lint-test command is not found in PATH

    Note:
        This function uses subprocess.run() with PIPE to capture output.
        While CLAUDE.md recommends RunningProcess.run_streaming() for long-running
        processes, we need to capture the full output here for ERROR.log and
        validation purposes. The output buffer (typically 64KB) should be sufficient
        for most lint-test runs, but very large outputs could potentially stall.
        Future improvement: Use streaming with a custom callback that accumulates output.
    """
    # Use shutil.which to find lint-test in PATH
    lint_test_path = shutil.which("lint-test")

    if lint_test_path is None:
        raise FileNotFoundError("lint-test command not found in PATH. Please ensure lint-test is installed and available in your PATH.")

    # Run lint-test with output capture
    # Note: We capture output here because we need to:
    # 1. Display it to the user
    # 2. Save it to ERROR.log file
    # 3. Check the return code
    result = subprocess.run(
        [lint_test_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        encoding="utf-8",
        errors="replace",  # Replace undecodable bytes with � instead of raising exception
        check=False,
    )

    return result.returncode, result.stdout


def _run_loop(args: Args, claude_path: str, loop_count: int) -> int:
    """Run Claude in a loop, checking for DONE.md after each iteration."""
    agent_task_dir = Path(".agent_task")

    # Handle existing session from previous run
    should_continue, start_iteration = _handle_existing_agent_task(agent_task_dir)
    if not should_continue:
        return 2  # User cancelled

    # Create .agent_task directory if it doesn't exist (may have been deleted)
    agent_task_dir.mkdir(exist_ok=True)

    # DONE.md lives at project root, not .agent_task/
    done_file = Path("DONE.md")

    # Initialize or load task info
    info_file = agent_task_dir / "info.json"
    user_prompt = args.prompt if args.prompt else args.message
    task_info = TaskInfo.load(info_file)

    if task_info is None:
        # Create new task info for fresh session
        task_info = TaskInfo(
            session_id=str(uuid.uuid4()),
            start_time=time.time(),
            prompt=user_prompt,
            total_iterations=loop_count,
        )
        task_info.save(info_file)
    else:
        # Update existing task info for continuation
        task_info.total_iterations = loop_count
        task_info.save(info_file)

    # Start from determined iteration (may be > 1 if continuing previous session)
    for i in range(start_iteration - 1, loop_count):
        iteration_num = i + 1
        print(f"\n--- Iteration {iteration_num}/{loop_count} ---", file=sys.stderr)

        # Check if DONE.md was already validated in a previous iteration
        done_validated_marker = agent_task_dir / "done_validated"
        if done_validated_marker.exists():
            print("✅ DONE.md was already validated. Halting immediately.", file=sys.stderr)
            print(f"Opening {done_file}...", file=sys.stderr)
            _open_file_in_editor(done_file)
            return 0

        # Mark iteration start
        task_info.start_iteration(iteration_num)
        task_info.save(info_file)

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
        # Wrap command in git-bash on Windows if available
        cmd = _wrap_command_for_git_bash(cmd)

        # Detect and print model message (for display only)
        model_flag = _get_model_from_args(args.claude_args)
        _print_model_message(model_flag)

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Execute the command with streaming if prompt is present
        if args.prompt:
            if args.plain:
                # Plain mode: no JSON formatting, just pass through output
                returncode = RunningProcess.run_streaming(cmd)
            else:
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

        # Mark iteration end
        error_msg = f"Exit code: {returncode}" if returncode != 0 else None
        task_info.end_iteration(returncode, error_msg)
        task_info.save(info_file)

        if returncode != 0 and args.verbose:
            print(f"Warning: Iteration {iteration_num} exited with code {returncode}", file=sys.stderr)

        # Check if DONE.md was created (at project root)
        # FSM State: DONE.md exists → enter validation/fix loop (never delete DONE.md)
        if done_file.exists():
            # Validate that lint and test pass before accepting DONE.md
            print(f"\n📋 DONE.md detected at project root after iteration {iteration_num}.", file=sys.stderr)
            print("Validating with `lint-test`...", file=sys.stderr)

            # Error log file for validation failures
            error_log_file = agent_task_dir / "ERROR.log"

            # Run lint-test and capture output
            try:
                # Find and run lint-test using shutil.which for validation
                lint_test_returncode, lint_test_output = _find_and_run_lint_test()

                # Display output to user
                print(lint_test_output)

                if lint_test_returncode != 0:
                    # FSM State: Validation failed → enter fix loop (keep DONE.md)
                    print("❌ lint-test failed. Keeping DONE.md and attempting to fix...", file=sys.stderr)

                    # Save full output to ERROR.log (with tee-like behavior - already printed above)
                    error_log_file.write_text(
                        f"# Lint-Test Validation Errors\n\nTimestamp: {time.strftime('%Y-%m-%d %H:%M:%S')}\nIteration: {iteration_num}/{loop_count}\n\n```\n{lint_test_output}\n```\n",
                        encoding="utf-8",
                    )
                    print(f"  Saved validation output to {error_log_file}", file=sys.stderr)

                    # FSM State: Fix loop (max 3 attempts, not 5)
                    max_fix_attempts = 3
                    retest_returncode: int = 1  # Initialize as failed
                    for fix_attempt in range(1, max_fix_attempts + 1):
                        print(f"\n🔧 Fix attempt {fix_attempt}/{max_fix_attempts}...", file=sys.stderr)

                        # Build fix prompt referencing ERROR.log and lint-test command
                        fix_prompt = (
                            "Read .agent_task/ERROR.log to see the linting and testing errors. "
                            "Fix all the errors listed in ERROR.log. "
                            "You can run the `lint-test` command yourself to validate the errors and confirm they are fixed. "
                            "After fixing, the system will automatically re-run lint-test to verify."
                        )

                        # Build fix command (using -p flag for non-interactive)
                        fix_cmd = [claude_path, "--dangerously-skip-permissions", "-p", fix_prompt]

                        # Add model flag from args if specified (no default model)
                        fix_model_flag = _get_model_from_args(args.claude_args)
                        if args.claude_args:
                            fix_cmd.extend(args.claude_args)

                        if not args.plain:
                            fix_cmd.extend(["--output-format", "stream-json", "--verbose"])

                        # Print model message for fix attempt
                        _print_model_message(fix_model_flag)

                        # Execute fix command
                        if args.plain:
                            RunningProcess.run_streaming(fix_cmd)
                        else:
                            formatter = StreamJsonFormatter(
                                show_system=args.verbose,
                                show_usage=True,
                                show_cache=args.verbose,
                                verbose=args.verbose,
                            )
                            stdout_callback = create_formatter_callback(formatter)
                            RunningProcess.run_streaming(fix_cmd, stdout_callback=stdout_callback)

                        # Re-run lint-test to check if fixed
                        print(f"\n🔍 Re-running lint-test after fix attempt {fix_attempt}...", file=sys.stderr)
                        retest_returncode, retest_output = _find_and_run_lint_test()

                        # Display retest output
                        print(retest_output)

                        if retest_returncode == 0:
                            # FSM State: Validation passed → mark as complete and halt
                            print(f"✅ lint-test passed after {fix_attempt} fix attempt(s)!", file=sys.stderr)

                            # Clean up ERROR.log since validation passed
                            if error_log_file.exists():
                                error_log_file.unlink()
                                print(f"  Removed {error_log_file}", file=sys.stderr)

                            # Accept DONE.md and mark as validated
                            task_info.mark_completed()
                            task_info.save(info_file)
                            done_validated_marker.write_text(
                                f"DONE.md validated successfully on {time.strftime('%Y-%m-%d %H:%M:%S')}\nIteration: {iteration_num}/{loop_count}\nFix attempts: {fix_attempt}\n",
                                encoding="utf-8",
                            )
                            break
                        else:
                            # FSM State: Still failing → update ERROR.log for next attempt
                            print(f"❌ lint-test still failing after fix attempt {fix_attempt}", file=sys.stderr)

                            # Update ERROR.log with latest output
                            error_log_file.write_text(
                                f"# Lint-Test Validation Errors (Attempt {fix_attempt})\n\n"
                                f"Timestamp: {time.strftime('%Y-%m-%d %H:%M:%S')}\n"
                                f"Iteration: {iteration_num}/{loop_count}\n"
                                f"Fix Attempt: {fix_attempt}/{max_fix_attempts}\n\n"
                                f"```\n{retest_output}\n```\n",
                                encoding="utf-8",
                            )

                            if fix_attempt == max_fix_attempts:
                                # FSM State: Max attempts reached → halt with warning (keep DONE.md)
                                _print_red_banner("LINTING/TESTING ISSUES REMAIN UNRESOLVED AFTER 3 FIX ATTEMPTS")
                                print(f"\nERROR: Failed to fix lint/test errors after {max_fix_attempts} attempts.", file=sys.stderr)
                                print("Please review .agent_task/ERROR.log manually.", file=sys.stderr)
                                print("DONE.md is kept at project root for review.", file=sys.stderr)
                                print("Halting loop - linting & testing could not pass.", file=sys.stderr)
                                # NEVER delete DONE.md - keep it along with ERROR.log for manual review

                    # If we get here and retest_returncode == 0, we fixed it successfully
                    if retest_returncode == 0:
                        break  # Exit main loop - validation passed
                    else:
                        # FSM State: Still broken after max attempts - HALT (keep DONE.md)
                        # This prevents infinite loops and wasted API credits
                        print(f"\n⚠️  Halting loop after {max_fix_attempts} failed fix attempts.", file=sys.stderr)
                        print("Review DONE.md and .agent_task/ERROR.log to understand the issues.", file=sys.stderr)
                        break
                else:
                    # FSM State: Validation passed on first attempt → accept DONE.md and halt
                    print("✅ lint-test passed. Accepting DONE.md and halting early.", file=sys.stderr)
                    task_info.mark_completed()
                    task_info.save(info_file)
                    done_validated_marker.write_text(
                        f"DONE.md validated successfully on {time.strftime('%Y-%m-%d %H:%M:%S')}\nIteration: {iteration_num}/{loop_count}\nValidated on first attempt (no fixes needed)\n",
                        encoding="utf-8",
                    )
                    break
            except KeyboardInterrupt:
                raise  # Always re-raise KeyboardInterrupt
            except Exception as e:
                # Validation failed due to exception (e.g., encoding error)
                print(f"Error during lint-test validation: {e}", file=sys.stderr)
                _print_red_banner("DONE.md VALIDATION FAILED DUE TO ERROR")
                print(f"Exception type: {type(e).__name__}", file=sys.stderr)
                print(f"Exception details: {e}", file=sys.stderr)
                print(file=sys.stderr)
                print("⚠️  DONE.md will be KEPT and loop will HALT to prevent infinite loop.", file=sys.stderr)
                print("Review the error above and fix the issue manually.", file=sys.stderr)
                print("Possible causes:", file=sys.stderr)
                print("  - Encoding errors in lint-test output (see BUG_CHARMAP.md)", file=sys.stderr)
                print("  - lint-test command not found or failed to execute", file=sys.stderr)
                print("  - Permission errors reading/writing files", file=sys.stderr)
                print(file=sys.stderr)
                print(f"DONE.md location: {done_file.absolute()}", file=sys.stderr)

                # Mark as completed (even though validation failed) to prevent re-running
                task_info.mark_completed(error=f"Validation failed with exception: {e}")
                task_info.save(info_file)

                # Create a marker file to indicate validation was attempted but failed
                validation_failed_marker = agent_task_dir / "done_validation_failed"
                validation_failed_marker.write_text(
                    f"DONE.md validation failed on {time.strftime('%Y-%m-%d %H:%M:%S')}\n"
                    f"Iteration: {iteration_num}/{loop_count}\n"
                    f"Exception: {type(e).__name__}: {e}\n"
                    f"\n"
                    f"IMPORTANT: DONE.md was KEPT to prevent infinite loop.\n"
                    f"Fix the validation issue manually and re-run if needed.\n",
                    encoding="utf-8",
                )

                # HALT the loop (do NOT continue to next iteration)
                break

    print("\nAll iterations complete or halted early.", file=sys.stderr)

    # Mark completion if all iterations finish without DONE.md
    if not done_file.exists():
        task_info.mark_completed(error="Completed all iterations without DONE.md")
        task_info.save(info_file)

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

    # Generate unique instance ID for this agent run
    instance_id = str(uuid.uuid4())
    session_id = instance_id  # In standalone mode, session_id equals instance_id

    # Register hooks early (before any execution)
    register_hooks_from_config(instance_id=instance_id, session_id=session_id, hook_debug=args.hook_debug)

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
        # Get and set API key before launching Claude
        api_key = get_api_key(args)
        os.environ["ANTHROPIC_API_KEY"] = api_key
        os.environ["CLAUDE_CODE_MAX_OUTPUT_TOKENS"] = "16000"

        # No validation needed - if no input is provided and stdin is a tty,
        # Claude Code will launch in interactive mode

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
                # Enable streaming JSON output for -p flag by default (unless --plain is used)
                # Note: stream-json requires --verbose when used with --print/-p
                if not args.plain:
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
            # Claude Code not found - offer to install it locally
            print("Error: Claude Code is not installed or not in PATH", file=sys.stderr)
            print(file=sys.stderr)

            # Offer automatic installation
            if prompt_install_claude():
                # Installation succeeded, try finding it again
                claude_path = _find_claude_path()
                if not claude_path:
                    print("Error: Installation succeeded but claude executable still not found", file=sys.stderr)
                    return 1
            else:
                # Installation declined or failed
                print(file=sys.stderr)
                print("You can also:", file=sys.stderr)
                print("  - Install globally: npm install -g @anthropic-ai/claude-code@latest", file=sys.stderr)
                print("  - Install later with: clud --install-claude", file=sys.stderr)
                print("  - Download from: https://claude.ai/download", file=sys.stderr)
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
                    # Not an integer, check if it's a file path
                    # File paths (especially .md files) get expanded to a template message
                    if args.loop_value.endswith(".md") or Path(args.loop_value).exists():
                        # Expand to template message for file-based loop mode
                        loop_message = (
                            f"Read {args.loop_value} and do the next task. "
                            f"You are free to update {args.loop_value} with information critical "
                            f"for the next agent and future agents as this task is worked on."
                        )
                    else:
                        # Not a file path, treat as regular message
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
        # Wrap command in git-bash on Windows if available
        cmd = _wrap_command_for_git_bash(cmd)

        # Detect and print model message (for display only)
        model_flag = _get_model_from_args(args.claude_args)
        _print_model_message(model_flag)

        # Print debug info
        _print_debug_info(claude_path, cmd, args.verbose)

        # Trigger AGENT_START hook
        user_message = args.prompt if args.prompt else args.message if args.message else None
        trigger_hook_sync(
            HookEvent.AGENT_START,
            HookContext(
                event=HookEvent.AGENT_START,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                message=user_message,
            ),
            hook_debug=args.hook_debug,
        )

        # Execute Claude with the dangerous permissions flag
        # Use idle detection if timeout is specified
        returncode = 0  # Initialize returncode for hook triggers
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
            returncode = 0
        elif args.prompt:
            # Use RunningProcess for streaming output when using -p flag
            # This ensures stream-json output is displayed line-by-line in real-time
            if args.plain:
                # Plain mode: no JSON formatting, just pass through output
                returncode = RunningProcess.run_streaming(cmd)
            else:
                # Create JSON formatter for beautiful output
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

        # Trigger POST_EXECUTION hook after successful completion
        trigger_hook_sync(
            HookEvent.POST_EXECUTION,
            HookContext(
                event=HookEvent.POST_EXECUTION,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                message=user_message,
                metadata={"returncode": returncode},
            ),
            hook_debug=args.hook_debug,
        )

        return returncode

    except FileNotFoundError as e:
        error_msg = f"Claude Code is not installed or not in PATH: {e}"
        print(f"Error: {error_msg}", file=sys.stderr)
        print("Install Claude Code from: https://claude.ai/download", file=sys.stderr)
        print(f"DEBUG: FileNotFoundError details: {e}", file=sys.stderr)
        traceback.print_exc()

        # Trigger ERROR hook
        trigger_hook_sync(
            HookEvent.ERROR,
            HookContext(
                event=HookEvent.ERROR,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                error=error_msg,
            ),
            hook_debug=args.hook_debug,
        )
        return 1

    except KeyboardInterrupt:
        print("\nInterrupted by user", file=sys.stderr)

        # Trigger AGENT_STOP hook on interrupt
        trigger_hook_sync(
            HookEvent.AGENT_STOP,
            HookContext(
                event=HookEvent.AGENT_STOP,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                metadata={"reason": "interrupted"},
            ),
            hook_debug=args.hook_debug,
        )
        return 130

    except OSError as e:
        error_msg = f"OS error launching Claude: {e}"

        # Try backup method with shell=True first (Windows shell script issue)
        # Only show error if backup also fails
        if cmd and claude_path:
            try:
                # Silently try shell=True method first
                if args.verbose:
                    print(f"DEBUG: OSError {e.winerror if hasattr(e, 'winerror') else e.errno}, retrying with shell=True...", file=sys.stderr)
                return _execute_command(cmd, use_shell=True, verbose=args.verbose)
            except Exception as shell_error:
                # Both methods failed - now show full error details
                print(f"Error launching Claude: {e}", file=sys.stderr)
                print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
                _print_error_diagnostics(claude_path, cmd)
                print(f"\nBackup method also failed: {shell_error}", file=sys.stderr)
                traceback.print_exc()
                # Fall through to trigger ERROR hook and return 1
        else:
            # Can't attempt backup
            print(f"Error launching Claude: {e}", file=sys.stderr)
            print(f"DEBUG: OSError details - errno: {e.errno}, winerror: {getattr(e, 'winerror', 'N/A')}", file=sys.stderr)
            _print_error_diagnostics(claude_path, cmd)
            print("\nFull stack trace from original error:", file=sys.stderr)
            traceback.print_exc()
            # Fall through to trigger ERROR hook and return 1

        # Trigger ERROR hook (reached when both methods fail or backup can't be attempted)
        trigger_hook_sync(
            HookEvent.ERROR,
            HookContext(
                event=HookEvent.ERROR,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                error=error_msg,
            ),
            hook_debug=args.hook_debug,
        )
        return 1

    except Exception as e:
        error_msg = f"Unexpected error launching Claude: {e}"
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

        # Trigger ERROR hook
        trigger_hook_sync(
            HookEvent.ERROR,
            HookContext(
                event=HookEvent.ERROR,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
                error=error_msg,
            ),
            hook_debug=args.hook_debug,
        )
        return 1

    finally:
        # Send cleanup notification if telegram bot is available
        if telegram_bot:
            telegram_bot.send_cleanup()

        # Trigger AGENT_STOP hook in finally block
        trigger_hook_sync(
            HookEvent.AGENT_STOP,
            HookContext(
                event=HookEvent.AGENT_STOP,
                instance_id=instance_id,
                session_id=session_id,
                client_type="cli",
                client_id="standalone",
            ),
            hook_debug=args.hook_debug,
        )


def main(args_list: list[str] | None = None) -> int:
    """Main entry point for clud - handles routing and execution."""
    try:
        # Set terminal title early
        # DISABLED: Causes escape sequence artifacts on some terminals (git-bash/mintty)
        # set_terminal_title()

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
            print("  --lint               Run lint and tests with lint-test")
            print("  --test               Run lint and tests with lint-test")
            print("  --fix [URL]          Fix linting issues and run tests (optionally from GitHub URL)")
            print("  --init-loop          Create LOOP.md index from existing markdown files")
            print("  --kanban             Launch vibe-kanban board (installs Node 22 if needed)")
            print("  --telegram, -tg      Open Telegram bot landing page in browser")
            print("  --telegram-server [PORT] [--telegram-config PATH]")
            print("                       Launch advanced Telegram integration server (default port: 8889)")
            print("  --webui, --ui [PORT] Launch Claude Code Web UI in browser (default port: 8888)")
            print("  --api-server [PORT]  Launch Message Handler API server (default port: 8765)")
            print("  --info               Show Claude Code installation information")
            print("  --install-claude     Install Claude Code to ~/.clud/npm (self-contained)")
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

        if args.telegram_server:
            return handle_telegram_server_command(args.telegram_server_port, args.telegram_server_config)

        if args.webui:
            return handle_webui_command(args.webui_port)

        if args.api_server:
            return handle_api_server_command(args.api_port)

        if args.code:
            return handle_code_command(args.code_port)

        if args.fix:
            return handle_fix_command(args.fix_url)

        if args.init_loop:
            return handle_init_loop_command()

        if args.info:
            return handle_info_command()

        if args.install_claude:
            return handle_install_claude_command()

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
                # Configure logging for tracking based on verbose flag
                import logging

                log_level = logging.DEBUG if args.verbose else logging.INFO
                logging.basicConfig(
                    level=log_level,
                    format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
                    force=True,  # Override any existing config
                )

                logger = logging.getLogger(__name__)
                logger.debug("Tracking enabled with debug logging")
                logger.debug(f"Command: {args.prompt or args.message or 'claude code'}")
                logger.debug(f"Verbose mode: {args.verbose}")

                from .agent.tracking import create_tracker

                # Get command from args or use default description
                command = args.prompt or args.message or "claude code"
                logger.debug(f"Creating tracker for command: {command}")
                tracker = create_tracker(command)

                exit_code = 1  # Default exit code in case of exception
                try:
                    logger.debug("Starting agent execution")
                    exit_code = run_agent(args)
                    logger.debug(f"Agent execution completed with exit code: {exit_code}")
                finally:
                    logger.debug("Stopping tracker")
                    tracker.stop(exit_code)
                    logger.debug("Tracker stopped")

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
