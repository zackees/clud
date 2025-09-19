"""Minimal CLI entry point for clud - routes to appropriate agent modules."""

import argparse
import sys
from pathlib import Path

from .agent_background import (
    BackgroundAgentArgs,
    ConfigError,
    DockerError,
    ValidationError,
    build_docker_image,
    check_docker_available,
    launch_container_shell,
    pull_latest_image,
    run_ui_container,
    validate_path,
)
from .agent_background_args import should_auto_build
from .agent_foreground import ConfigError as ForegroundConfigError
from .agent_foreground import ValidationError as ForegroundValidationError
from .agent_foreground import get_api_key, handle_login
from .git_worktree import (
    cleanup_git_worktree,
    create_git_worktree_in_container,
    list_git_worktrees_in_container,
    prune_git_worktrees_in_container,
    remove_git_worktree_in_container,
)
from .secrets import get_credential_store
from .task import handle_task_command

# Import keyring for tests to mock
keyring = get_credential_store()


def convert_to_background_args(parsed_args: argparse.Namespace, validate_path_exists: bool = True) -> BackgroundAgentArgs:
    """Convert argparse.Namespace to BackgroundAgentArgs."""
    if validate_path_exists:
        project_path = validate_path(parsed_args.path)
        host_dir = str(project_path)
    else:
        # For testing - don't validate path existence
        host_dir = parsed_args.path or str(Path.cwd())

    return BackgroundAgentArgs(
        host_dir=host_dir,
        sync_interval=300,  # default
        watch=False,  # default
        verbose=False,  # default
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
        cmd=getattr(parsed_args, "cmd", None),
        claude_commands=parsed_args.claude_commands,
        dump_threads_after=parsed_args.dump_threads_after,
        no_dangerous=parsed_args.no_dangerous,
        yolo=parsed_args.yolo,
        build_dockerfile=getattr(parsed_args, "build_dockerfile", None),
        worktree_name=parsed_args.worktree_name,
        detect_completion=parsed_args.detect_completion,
        idle_timeout=parsed_args.idle_timeout,
        _image_built=getattr(parsed_args, "_image_built", False),
    )


def create_parser() -> argparse.ArgumentParser:
    """Create the argument parser for clud."""
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

    parser.add_argument("--dump-threads-after", type=int, metavar="SECONDS", help="Dump thread information after specified seconds (for --bg mode)")

    parser.add_argument("-p", "--prompt", help="Run Claude with this prompt and exit when complete")

    # Git worktree management
    parser.add_argument("--worktree-create", metavar="BRANCH", help="Create a Git worktree for the specified branch inside Docker container")

    parser.add_argument("--worktree-new", metavar="BRANCH", help="Create a Git worktree with a new branch inside Docker container")

    parser.add_argument("--worktree-remove", action="store_true", help="Remove the Git worktree inside Docker container")

    parser.add_argument("--worktree-list", action="store_true", help="List Git worktrees inside Docker container")

    parser.add_argument("--worktree-prune", action="store_true", help="Prune stale Git worktree entries inside Docker container")

    parser.add_argument("--worktree-cleanup", action="store_true", help="Clean up worktree: remove worktree, prune entries, and delete directory")

    parser.add_argument("--worktree-name", default="worktree", help="Name of the worktree subdirectory (default: worktree)")

    parser.add_argument("-h", "--help", action="store_true", help="Show this help message and exit")

    return parser


def main(args: list[str] | None = None) -> int:
    """Main entry point for clud."""
    parser = create_parser()
    parsed_args, unknown_args = parser.parse_known_args(args)

    # Handle conflicting firewall options
    if parsed_args.no_firewall:
        parsed_args.enable_firewall = False

    # Track if build was explicitly requested by user
    parsed_args._original_build = parsed_args.build

    # Auto-build detection for clud repo directory
    if should_auto_build(parsed_args):
        print("Detected clud repository - auto-building Docker image...")
        parsed_args.build = True

    try:
        # Handle help flag - only show main help if no agent-specific flags are given
        if parsed_args.help and not (parsed_args.bg or parsed_args.task):
            parser.print_help()
            return 0

        # Handle login command first (doesn't need Docker)
        if parsed_args.login:
            return handle_login()

        # Handle task command (doesn't need Docker)
        if parsed_args.task:
            return handle_task_command(parsed_args.task)

        # Handle Git worktree commands (need Docker)
        worktree_commands = [parsed_args.worktree_create, parsed_args.worktree_new, parsed_args.worktree_remove, parsed_args.worktree_list, parsed_args.worktree_prune, parsed_args.worktree_cleanup]
        if any(worktree_commands):
            # Check Docker availability for worktree operations
            if not check_docker_available():
                raise DockerError("Docker is not available or not running")

            # Validate project path
            project_path = validate_path(parsed_args.path)

            # Handle each worktree command
            if parsed_args.worktree_create:
                success = create_git_worktree_in_container(project_path, parsed_args.worktree_create, parsed_args.worktree_name)
                return 0 if success else 1

            elif parsed_args.worktree_new:
                success = create_git_worktree_in_container(project_path, parsed_args.worktree_new, parsed_args.worktree_name, create_new_branch=True)
                return 0 if success else 1

            elif parsed_args.worktree_remove:
                success = remove_git_worktree_in_container(project_path, parsed_args.worktree_name)
                return 0 if success else 1

            elif parsed_args.worktree_list:
                result = list_git_worktrees_in_container(project_path)
                if result is not None:
                    print("Git worktrees:")
                    print(result)
                    return 0
                else:
                    return 1

            elif parsed_args.worktree_prune:
                success = prune_git_worktrees_in_container(project_path)
                return 0 if success else 1

            elif parsed_args.worktree_cleanup:
                success = cleanup_git_worktree(project_path, parsed_args.worktree_name)
                return 0 if success else 1

        # Handle background shell mode
        if parsed_args.bg:
            # Background shell mode - launch interactive bash shell with enforced entrypoint
            # Check Docker availability first
            if not check_docker_available():
                raise DockerError("Docker is not available or not running")

            # Import TTY validation here to avoid circular imports
            from .agent_background import validate_tty_for_interactive_mode

            # Validate TTY availability for interactive mode (unless specific command is provided)
            if not parsed_args.cmd or parsed_args.cmd == "/bin/bash":
                validate_tty_for_interactive_mode()

            # Use provided cmd or default to /bin/bash for workspace interaction
            if not parsed_args.cmd:
                parsed_args.cmd = "/bin/bash"

            # Build image if needed
            if (not hasattr(parsed_args, "_image_built") or not parsed_args._image_built) and not build_docker_image(getattr(parsed_args, "build_dockerfile", None)):
                return 1

            # Get API key and launch container with enforced entrypoint
            api_key = get_api_key(parsed_args)
            # Convert to BackgroundAgentArgs
            bg_args = convert_to_background_args(parsed_args)
            return launch_container_shell(bg_args, api_key)

        # Check if this is yolo mode (only if no Docker flags AND no path provided, OR if prompt is specified)
        # If a path is provided, default to Docker shell mode unless prompt is specified
        # Special case: if auto-build was triggered but no explicit path was provided, prefer yolo mode
        explicit_docker_mode = parsed_args.ui or parsed_args.update or parsed_args.just_build or parsed_args.bg
        explicit_build_requested = getattr(parsed_args, "_original_build", False)  # Track if build was explicitly requested

        is_yolo_mode = (not explicit_docker_mode and not explicit_build_requested and not parsed_args.path) or parsed_args.prompt

        if is_yolo_mode:
            # Handle yolo mode (doesn't need Docker)
            from .agent_foreground import main as yolo_main

            # Construct arguments for yolo main
            yolo_args: list[str] = []
            if parsed_args.prompt:
                yolo_args.extend(["-p", parsed_args.prompt])
            if parsed_args.message:
                yolo_args.extend(["-m", parsed_args.message])
            if parsed_args.dry_run:
                yolo_args.append("--dry-run")

            # Add unknown args to pass through to the foreground agent
            yolo_args.extend(unknown_args)

            return yolo_main(yolo_args)

        # From here on, we're in Docker-based mode
        # Check Docker availability first for all Docker-based modes
        if not check_docker_available():
            raise DockerError("Docker is not available or not running")

        # Handle update mode - always pull from remote registry
        if parsed_args.update:
            print("Updating clud runtime...")

            # Determine which image to pull
            # User specified a custom image, otherwise use the standard Claude Code image from Docker Hub
            # This ensures we always pull from remote, not build locally
            image_to_pull = parsed_args.image or "niteris/clud:latest"

            print(f"Pulling the latest version of {image_to_pull}...")

            if pull_latest_image(image_to_pull):
                print(f"Successfully updated {image_to_pull}")
                print("You can now run 'clud' to use the updated runtime.")

                # If they pulled a non-default image, remind them to use --image flag
                if image_to_pull != "niteris/clud:latest" and not parsed_args.image:
                    print(f"Note: To use this image, run: clud --image {image_to_pull}")

                return 0
            else:
                print(f"Failed to update {image_to_pull}", file=sys.stderr)
                print("Please check your internet connection and Docker configuration.")
                return 1

        # Handle build-only mode
        if parsed_args.just_build:
            force_rebuild = parsed_args.build == "force"
            if force_rebuild:
                print("Force building Docker image (no cache)...")
            else:
                print("Building Docker image...")
            if build_docker_image(getattr(parsed_args, "build_dockerfile", None), force_rebuild=force_rebuild, skip_existing_check=True):
                print("Docker image built successfully!")
                return 0
            else:
                print("Failed to build Docker image", file=sys.stderr)
                return 1

        # Force build if requested
        if parsed_args.build:
            force_rebuild = parsed_args.build == "force"
            if force_rebuild:
                print("Force building Docker image (no cache)...")
            else:
                print("Building Docker image...")
            if not build_docker_image(getattr(parsed_args, "build_dockerfile", None), force_rebuild=force_rebuild, skip_existing_check=True):
                print("Failed to build Docker image", file=sys.stderr)
                return 1
            parsed_args._image_built = True

        # Route to Docker-based modes
        if parsed_args.ui:
            # UI mode - launch code-server container
            project_path = validate_path(parsed_args.path)
            api_key = get_api_key(parsed_args)

            # Convert to BackgroundAgentArgs
            bg_args = convert_to_background_args(parsed_args)
            return run_ui_container(bg_args, project_path, api_key)
        else:
            # Container shell mode - launch container with interactive shell
            api_key = get_api_key(parsed_args)
            # Convert to BackgroundAgentArgs
            bg_args = convert_to_background_args(parsed_args)
            return launch_container_shell(bg_args, api_key)

    except (ValidationError, ForegroundValidationError) as e:
        print(f"Error: {e}", file=sys.stderr)
        return 2
    except DockerError as e:
        print(f"Docker error: {e}", file=sys.stderr)
        return 3
    except (ConfigError, ForegroundConfigError) as e:
        print(f"Configuration error: {e}", file=sys.stderr)
        return 4
    except KeyboardInterrupt:
        print("\nOperation cancelled.", file=sys.stderr)
        return 2
    except Exception as e:
        print(f"Unexpected error: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
