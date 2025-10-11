#!/usr/bin/env python3
"""Argument parsing for the background agent."""

import argparse
import logging
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass
class BackgroundAgentArgs:
    """Typed arguments for the background agent."""

    host_dir: str
    sync_interval: int
    watch: bool
    verbose: bool
    # Core arguments
    path: str | None = None  # Project directory to mount
    # Container configuration
    ssh_keys: bool = False
    image: str | None = None
    shell: str = "bash"
    profile: str = "python"
    enable_firewall: bool = True
    no_firewall: bool = False
    no_sudo: bool = False
    env: list[str] | None = None
    read_only_home: bool = False
    port: int = 8743
    cmd: str | None = None
    claude_commands: str | None = None
    dump_threads_after: int | None = None
    # Permission modes
    no_dangerous: bool = False
    yolo: bool = False
    # Build control
    _image_built: bool = False
    build_dockerfile: str | None = None
    # Git worktree
    worktree_name: str = "worktree"
    # Completion detection
    detect_completion: bool = False
    idle_timeout: float = 3.0
    # Browser opening for VS Code server
    open: bool = False
    # Telegram messaging configuration
    telegram_enabled: bool = False
    telegram_bot_token: str | None = None
    telegram_chat_id: str | None = None


logger = logging.getLogger(__name__)


def is_clud_repo_directory(path: Path | None = None) -> bool:
    """Check if the current or specified directory is the clud repository."""
    path = Path.cwd() if path is None else Path(path)

    # Check for key files that indicate this is the clud repo
    pyproject_path = path / "pyproject.toml"
    clud_init_path = path / "src" / "clud" / "__init__.py"

    if not (pyproject_path.exists() and clud_init_path.exists()):
        return False

    # Verify pyproject.toml contains clud project
    try:
        with open(pyproject_path, encoding="utf-8") as f:
            content = f.read()
            return 'name = "clud"' in content and 'description = "Claude in a Docker Box"' in content
    except (OSError, UnicodeDecodeError):
        return False


def should_auto_build(parsed_args: argparse.Namespace) -> bool:
    """Determine if auto-build should be triggered for clud repo directory."""
    # Auto-build detection for clud repo directory
    project_path = Path(parsed_args.path) if parsed_args.path else Path.cwd()
    is_clud_repo = is_clud_repo_directory(project_path)

    # Auto-build only when --bg flag is present and in clud repo directory
    return is_clud_repo and getattr(parsed_args, "bg", False)


def parse_background_agent_args(args: list[str] | None = None) -> BackgroundAgentArgs:
    """Parse command line arguments into typed dataclass."""
    parser = argparse.ArgumentParser(description="CLUD background sync agent", add_help=False)

    # Set default directories - these are only used when BackgroundAgent runs inside container
    # When running from host (e.g., clud bg --cmd), these defaults are not used
    # because launch_container_shell() is called instead
    default_host_dir = "/host"

    parser.add_argument("path", nargs="?", help="Project directory to mount (default: current working directory)")
    parser.add_argument("--host-dir", default=default_host_dir, help=f"Host directory path (default: {default_host_dir})")
    # Note: workspace-dir is fixed at /workspace and should not be configurable
    parser.add_argument(
        "--sync-interval",
        type=int,
        default=300,
        help="Sync interval in seconds (default: 300)",
    )
    parser.add_argument(
        "--watch",
        action="store_true",
        help="Enable file watching mode (experimental)",
    )
    parser.add_argument("--verbose", action="store_true", help="Enable verbose logging")

    # Container configuration arguments
    parser.add_argument("--ssh-keys", action="store_true", help="Mount ~/.ssh read-only for git push or private repos")
    parser.add_argument("--image", help="Override container image")
    parser.add_argument("--shell", default="bash", help="Preferred shell inside container (default: bash)")
    parser.add_argument("--profile", default="python", help="Toolchain profile (default: python)")
    parser.add_argument("--enable-firewall", action="store_true", default=True, help="Enable container firewall (default: enabled)")
    parser.add_argument("--no-firewall", action="store_true", help="Disable container firewall")
    parser.add_argument("--no-sudo", action="store_true", help="Disable sudo privileges (enabled by default)")
    parser.add_argument("--env", action="append", help="Forward environment variables (KEY=VALUE, repeatable)")
    parser.add_argument("--read-only-home", action="store_true", help="Mount home directory read-only as /host-home")
    parser.add_argument("--port", type=int, default=8743, help="Port for code-server UI (default: 8743)")
    parser.add_argument("--cmd", help="Command to execute in container instead of interactive shell")
    parser.add_argument("--claude-commands", help="Path to directory or file containing Claude CLI plugins to mount into container")
    parser.add_argument("--dump-threads-after", type=int, metavar="SECONDS", help="Dump thread information after specified seconds (for --bg mode)")

    # Permission modes
    parser.add_argument("--no-dangerous", action="store_true", help="Disable skip permission prompts inside container (dangerous mode is default)")
    parser.add_argument("--yolo", action="store_true", help="Launch Claude Code with dangerous permissions (bypasses all safety prompts)")

    # Build control
    parser.add_argument("--build-dockerfile", metavar="PATH", help="Build Docker image using custom dockerfile path")

    # Git worktree
    parser.add_argument("--worktree-name", default="worktree", help="Name of the worktree subdirectory (default: worktree)")

    # Completion detection
    parser.add_argument("--detect-completion", action="store_true", help="Monitor terminal for agent completion (3-second idle detection)")
    parser.add_argument("--idle-timeout", type=float, default=3.0, help="Timeout in seconds for agent completion detection (default: 3.0)")

    # Telegram notifications
    parser.add_argument("--telegram", action="store_true", help="Enable Telegram notifications")
    parser.add_argument("--telegram-bot-token", help="Telegram bot token (or use TELEGRAM_BOT_TOKEN env var)")
    parser.add_argument("--telegram-chat-id", help="Telegram chat ID to send messages to (or use TELEGRAM_CHAT_ID env var)")
    parser.add_argument("--open", action="store_true", help="Open VS Code server in browser")

    # Help
    parser.add_argument("-h", "--help", action="store_true", help="Show this help message and exit")

    parsed_args = parser.parse_args(args)

    # Handle conflicting firewall options
    if parsed_args.no_firewall:
        parsed_args.enable_firewall = False

    # Handle help
    if parsed_args.help:
        parser.print_help()
        sys.exit(0)

    # Import os for env var fallback
    import os

    # Determine Telegram enabled
    telegram_enabled = parsed_args.telegram or bool(parsed_args.telegram_bot_token) or bool(parsed_args.telegram_chat_id)

    # Get Telegram credentials from env vars as fallback
    telegram_bot_token = parsed_args.telegram_bot_token or os.environ.get("TELEGRAM_BOT_TOKEN")
    telegram_chat_id = parsed_args.telegram_chat_id or os.environ.get("TELEGRAM_CHAT_ID")

    return BackgroundAgentArgs(
        host_dir=parsed_args.host_dir,
        sync_interval=parsed_args.sync_interval,
        watch=parsed_args.watch,
        verbose=parsed_args.verbose,
        path=parsed_args.path,
        ssh_keys=parsed_args.ssh_keys,
        image=parsed_args.image,
        shell=parsed_args.shell,
        profile=parsed_args.profile,
        enable_firewall=parsed_args.enable_firewall,
        no_firewall=parsed_args.no_firewall,
        no_sudo=parsed_args.no_sudo,
        env=parsed_args.env,
        read_only_home=parsed_args.read_only_home,
        port=parsed_args.port,
        cmd=parsed_args.cmd,
        claude_commands=parsed_args.claude_commands,
        dump_threads_after=parsed_args.dump_threads_after,
        no_dangerous=parsed_args.no_dangerous,
        yolo=parsed_args.yolo,
        build_dockerfile=parsed_args.build_dockerfile,
        worktree_name=parsed_args.worktree_name,
        detect_completion=parsed_args.detect_completion,
        idle_timeout=parsed_args.idle_timeout,
        open=getattr(parsed_args, "open", False),
        telegram_enabled=telegram_enabled,
        telegram_bot_token=telegram_bot_token,
        telegram_chat_id=telegram_chat_id,
    )
