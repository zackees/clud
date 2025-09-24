#!/usr/bin/env python3
"""Type-safe CLI argument parsing for clud."""

import argparse
from dataclasses import dataclass
from typing import Any


@dataclass
class CliArgs:
    """Type-safe dataclass for all CLI arguments."""

    # Core arguments
    path: str | None = None
    login: bool = False

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

    # Permission modes
    no_dangerous: bool = False
    yolo: bool = False

    # API key management
    api_key: str | None = None
    api_key_from: str | None = None

    # Build and deployment
    build: Any = False  # Can be True, False, or "force"
    build_dockerfile: str | None = None
    just_build: bool = False
    update: bool = False

    # Runtime modes
    ui: bool = False
    bg: bool = False
    open: bool = False

    # Task and message handling
    task: str | None = None
    message: str | None = None
    prompt: str | None = None
    dry_run: bool = False

    # Completion detection
    detect_completion: bool = False
    idle_timeout: float = 3.0
    dump_threads_after: int | None = None

    # Git worktree management
    worktree_create: str | None = None
    worktree_new: str | None = None
    worktree_remove: bool = False
    worktree_list: bool = False
    worktree_prune: bool = False
    worktree_cleanup: bool = False
    worktree_name: str = "worktree"

    # Special commands
    lint: bool = False
    test: bool = False
    fix: bool = False

    # Utility
    help: bool = False
    version: bool = False

    # Internal state tracking
    _original_build: Any = False
    _image_built: bool = False


def parse_cli_args(args: list[str] | None = None) -> CliArgs:
    """Parse command line arguments into a type-safe CliArgs dataclass."""
    parser = argparse.ArgumentParser(
        prog="clud",
        description="Launch a Claude-powered development container",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        add_help=False,
    )

    parser.add_argument("path", nargs="?", help="Project directory to mount (default: current working directory)")

    parser.add_argument("--login", action="store_true", help="Configure API key for Claude")

    parser.add_argument("--no-dangerous", action="store_true", help="Disable skip permission prompts inside container (dangerous mode is default)")

    parser.add_argument("--ssh-keys", action="store_true", help="Mount ~/.ssh read-only for git push or private repos")

    parser.add_argument("--image", help="Override container image")

    parser.add_argument("--shell", default="bash", help="Preferred shell inside container (default: bash)")

    parser.add_argument("--profile", default="python", help="Toolchain profile (default: python)")

    parser.add_argument("--enable-firewall", action="store_true", default=True, help="Enable container firewall (default: enabled)")

    parser.add_argument("--no-firewall", action="store_true", help="Disable container firewall")

    parser.add_argument("--no-sudo", action="store_true", help="Disable sudo privileges (enabled by default)")

    parser.add_argument("--env", action="append", help="Forward environment variables (KEY=VALUE, repeatable)")

    parser.add_argument("--api-key-from", help="Retrieve ANTHROPIC_API_KEY from OS keyring entry NAME")

    parser.add_argument("--read-only-home", action="store_true", help="Mount home directory read-only as /host-home")

    parser.add_argument("--ui", action="store_true", help="Launch code-server UI in browser")

    parser.add_argument("--port", type=int, default=8743, help="Port for code-server UI (default: 8743)")

    parser.add_argument("--api-key", help="Anthropic API key for Claude CLI")

    parser.add_argument("-b", "--build", nargs="?", const=True, help="Build Docker image before launching container. Use --build=force to force rebuild without cache")

    parser.add_argument("--build-dockerfile", metavar="PATH", help="Build Docker image using custom dockerfile path")

    parser.add_argument("--just-build", action="store_true", help="Build Docker image and exit (don't launch container)")

    parser.add_argument("-u", "--update", action="store_true", help="Pull the latest Docker image and upgrade the runtime")

    parser.add_argument("--version", action="version", version="clud 0.0.1")

    parser.add_argument("--cmd", help="Command to execute in container instead of interactive shell")

    parser.add_argument("--claude-commands", help="Path to directory or file containing Claude CLI plugins to mount into container")

    parser.add_argument("--yolo", action="store_true", help="Launch Claude Code with dangerous permissions (bypasses all safety prompts)")

    parser.add_argument("-t", "--task", metavar="PATH", help="Open task file in editor and process tasks")

    parser.add_argument("-m", "--message", help="Send this message to Claude (strips the -m flag)")

    parser.add_argument("--dry-run", action="store_true", help="Print what would be executed without actually running Claude")

    parser.add_argument("--detect-completion", action="store_true", help="Monitor terminal for agent completion (3-second idle detection)")

    parser.add_argument("--idle-timeout", type=float, default=3.0, help="Timeout in seconds for agent completion detection (default: 3.0)")

    parser.add_argument("--bg", action="store_true", help="Launch interactive bash shell in workspace (enforces entrypoint)")

    parser.add_argument("-o", "--open", action="store_true", help="Open browser pointing to VS Code server (for --bg mode)")

    parser.add_argument("--dump-threads-after", type=int, metavar="SECONDS", help="Dump thread information after specified seconds (for --bg mode)")

    parser.add_argument("-p", "--prompt", help="Run Claude with this prompt and exit when complete")

    parser.add_argument("--lint", action="store_true", help="Run global linting with codeup and fix all errors")

    parser.add_argument("--test", action="store_true", help="Run tests with codeup and fix all failures")

    parser.add_argument("--fix", action="store_true", help="Run both linting and tests with codeup and fix all errors")

    # Git worktree management
    parser.add_argument("--worktree-create", metavar="BRANCH", help="Create a Git worktree for the specified branch inside Docker container")

    parser.add_argument("--worktree-new", metavar="BRANCH", help="Create a Git worktree with a new branch inside Docker container")

    parser.add_argument("--worktree-remove", action="store_true", help="Remove the Git worktree inside Docker container")

    parser.add_argument("--worktree-list", action="store_true", help="List Git worktrees inside Docker container")

    parser.add_argument("--worktree-prune", action="store_true", help="Prune stale Git worktree entries inside Docker container")

    parser.add_argument("--worktree-cleanup", action="store_true", help="Clean up worktree: remove worktree, prune entries, and delete directory")

    parser.add_argument("--worktree-name", default="worktree", help="Name of the worktree subdirectory (default: worktree)")

    parser.add_argument("-h", "--help", action="store_true", help="Show this help message and exit")

    parsed_args, unknown_args = parser.parse_known_args(args)

    # Handle conflicting firewall options
    if parsed_args.no_firewall:
        parsed_args.enable_firewall = False

    # Track if build was explicitly requested by user
    _original_build = parsed_args.build

    # Create the dataclass instance
    cli_args = CliArgs(
        path=parsed_args.path,
        login=parsed_args.login,
        ssh_keys=parsed_args.ssh_keys,
        image=parsed_args.image,
        shell=parsed_args.shell,
        profile=parsed_args.profile,
        enable_firewall=parsed_args.enable_firewall,
        no_firewall=parsed_args.no_firewall,
        no_sudo=parsed_args.no_sudo,
        env=parsed_args.env,
        api_key_from=parsed_args.api_key_from,
        read_only_home=parsed_args.read_only_home,
        ui=parsed_args.ui,
        port=parsed_args.port,
        api_key=parsed_args.api_key,
        build=parsed_args.build,
        build_dockerfile=parsed_args.build_dockerfile,
        just_build=parsed_args.just_build,
        update=parsed_args.update,
        cmd=parsed_args.cmd,
        claude_commands=parsed_args.claude_commands,
        no_dangerous=parsed_args.no_dangerous,
        yolo=parsed_args.yolo,
        task=parsed_args.task,
        message=parsed_args.message,
        dry_run=parsed_args.dry_run,
        detect_completion=parsed_args.detect_completion,
        idle_timeout=parsed_args.idle_timeout,
        bg=parsed_args.bg,
        open=parsed_args.open,
        dump_threads_after=parsed_args.dump_threads_after,
        prompt=parsed_args.prompt,
        lint=parsed_args.lint,
        test=parsed_args.test,
        fix=parsed_args.fix,
        worktree_create=parsed_args.worktree_create,
        worktree_new=parsed_args.worktree_new,
        worktree_remove=parsed_args.worktree_remove,
        worktree_list=parsed_args.worktree_list,
        worktree_prune=parsed_args.worktree_prune,
        worktree_cleanup=parsed_args.worktree_cleanup,
        worktree_name=parsed_args.worktree_name,
        help=parsed_args.help,
        _original_build=_original_build,
        _image_built=False,
    )

    return cli_args
