"""Minimal CLI entry point for clud - routes to appropriate agent modules."""

import argparse
import sys

from .agent_completion import detect_agent_completion
from .agent_background import (
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
from .agent_foreground import get_api_key, handle_login
from .task import handle_task_command


def create_parser() -> argparse.ArgumentParser:
    """Create the argument parser for clud."""
    parser = argparse.ArgumentParser(
        prog="clud",
        description="Launch a Claude-powered development container",
        formatter_class=argparse.RawDescriptionHelpFormatter,
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

    parser.add_argument("-b", "--build", action="store_true", help="Build Docker image before launching container")

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

    return parser


def main(args: list[str] | None = None) -> int:
    """Main entry point for clud."""
    parser = create_parser()
    parsed_args = parser.parse_args(args)

    # Handle conflicting firewall options
    if parsed_args.no_firewall:
        parsed_args.enable_firewall = False

    try:
        # Handle login command first (doesn't need Docker)
        if parsed_args.login:
            return handle_login()

        # Handle task command (doesn't need Docker)
        if parsed_args.task:
            return handle_task_command(parsed_args.task)

        # Handle yolo mode (doesn't need Docker)
        # Check if this is the default yolo mode (no specific flags for Docker-based modes)
        is_yolo_mode = not (parsed_args.ui or parsed_args.update or parsed_args.just_build or parsed_args.build)
        if is_yolo_mode:
            from .agent_foreground import main as yolo_main

            # Construct arguments for yolo main
            yolo_args = []
            if parsed_args.message:
                yolo_args.extend(["-m", parsed_args.message])
            if parsed_args.dry_run:
                yolo_args.append("--dry-run")

            return yolo_main(yolo_args)

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
            print("Building Docker image...")
            if build_docker_image(getattr(parsed_args, "build_dockerfile", None)):
                print("Docker image built successfully!")
                return 0
            else:
                print("Failed to build Docker image", file=sys.stderr)
                return 1

        # Force build if requested
        if parsed_args.build:
            print("Building Docker image...")
            if not build_docker_image(getattr(parsed_args, "build_dockerfile", None)):
                print("Failed to build Docker image", file=sys.stderr)
                return 1
            parsed_args._image_built = True

        # Route to Docker-based modes
        if parsed_args.ui:
            # UI mode - launch code-server container
            project_path = validate_path(parsed_args.path)
            api_key = get_api_key(parsed_args)

            return run_ui_container(parsed_args, project_path, api_key)
        else:
            # Container shell mode - launch container with interactive shell
            api_key = get_api_key(parsed_args)
            return launch_container_shell(parsed_args, api_key)

    except ValidationError as e:
        print(f"Error: {e}", file=sys.stderr)
        return 2
    except DockerError as e:
        print(f"Docker error: {e}", file=sys.stderr)
        return 3
    except ConfigError as e:
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
