"""Minimal CLI entry point for clud - routes to appropriate agent modules."""

import subprocess
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
from .agent_foreground import ConfigError as ForegroundConfigError
from .agent_foreground import ValidationError as ForegroundValidationError
from .agent_foreground import get_api_key, handle_login
from .cli_args import CliArgs, parse_cli_args
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


def should_auto_build_cli_args(cli_args: CliArgs) -> bool:
    """Determine if auto-build should be triggered for clud repo directory."""
    from .agent_background_args import is_clud_repo_directory

    # Auto-build detection for clud repo directory
    project_path = Path(cli_args.path) if cli_args.path else Path.cwd()
    is_clud_repo = is_clud_repo_directory(project_path)

    # Auto-build only when --bg flag is present and in clud repo directory
    return is_clud_repo and cli_args.bg


def get_api_key_from_cli_args(cli_args: CliArgs) -> str:
    """Get API key from CLI args, compatible with existing get_api_key function."""

    # Create a temporary namespace-like object that get_api_key expects
    class TempArgs:
        def __init__(self, cli_args: CliArgs):
            self.api_key = cli_args.api_key
            self.api_key_from = cli_args.api_key_from

    temp_args = TempArgs(cli_args)
    return get_api_key(temp_args)


def handle_lint_command() -> int:
    """Handle the --lint command by running clud with a message to run codeup linting."""
    lint_prompt = "run codeup --lint --dry-run, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"

    try:
        # Run clud with the lint message and idle timeout
        result = subprocess.run(
            ["clud", "-m", lint_prompt, "--idle-timeout", "3"],
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: clud command not found. Make sure it's installed and in your PATH.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud: {e}", file=sys.stderr)
        return 1


def handle_test_command() -> int:
    """Handle the --test command by running clud with a message to run codeup testing."""
    test_prompt = "run codeup --test --dry-run, if it succeeds halt. Else fix issues and re-run, do this up to 5 times or until it succeeds"

    try:
        # Run clud with the test message and idle timeout
        result = subprocess.run(
            ["clud", "-m", test_prompt, "--idle-timeout", "3"],
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: clud command not found. Make sure it's installed and in your PATH.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud: {e}", file=sys.stderr)
        return 1


def handle_fix_command() -> int:
    """Handle the --fix command by running clud with a message to run both linting and testing."""
    fix_prompt = (
        "run `codeup --lint --dry-run` upto 5 times, fixing on each time or until it passes. "
        "and if it succeed then run `codeup --test --dry-run` upto 5 times, fixing each time until it succeeds. "
        "Finally run `codeup --lint --dry-run` and fix until it passes (upto 5 times) then halt. "
        "If you run into a locked file then try two times, same with misc system error. Else halt."
    )

    try:
        # Run clud with the fix message and idle timeout
        result = subprocess.run(
            ["clud", "-m", fix_prompt, "--idle-timeout", "3"],
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        return result.returncode
    except FileNotFoundError:
        print("Error: clud command not found. Make sure it's installed and in your PATH.", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error running clud: {e}", file=sys.stderr)
        return 1


def convert_to_background_args(cli_args: CliArgs, validate_path_exists: bool = True) -> BackgroundAgentArgs:
    """Convert CliArgs to BackgroundAgentArgs."""
    if validate_path_exists:
        project_path = validate_path(cli_args.path)
        host_dir = str(project_path)
    else:
        # For testing - don't validate path existence
        host_dir = cli_args.path or str(Path.cwd())

    return BackgroundAgentArgs(
        host_dir=host_dir,
        sync_interval=300,  # default
        watch=False,  # default
        verbose=False,  # default
        path=cli_args.path,
        ssh_keys=cli_args.ssh_keys,
        image=cli_args.image,
        shell=cli_args.shell,
        profile=cli_args.profile,
        enable_firewall=cli_args.enable_firewall,
        no_firewall=cli_args.no_firewall,
        no_sudo=cli_args.no_sudo,
        env=cli_args.env,
        read_only_home=cli_args.read_only_home,
        port=cli_args.port,
        cmd=cli_args.cmd,
        claude_commands=cli_args.claude_commands,
        dump_threads_after=cli_args.dump_threads_after,
        no_dangerous=cli_args.no_dangerous,
        yolo=cli_args.yolo,
        build_dockerfile=cli_args.build_dockerfile,
        worktree_name=cli_args.worktree_name,
        detect_completion=cli_args.detect_completion,
        idle_timeout=cli_args.idle_timeout,
        open=cli_args.open,
        _image_built=cli_args._image_built,
    )


def main(args: list[str] | None = None) -> int:
    """Main entry point for clud."""
    cli_args = parse_cli_args(args)

    # Auto-build detection for clud repo directory
    if should_auto_build_cli_args(cli_args):
        print("Detected clud repository - auto-building Docker image...")
        cli_args.build = True

    try:
        # Handle help flag - only show main help if no agent-specific flags are given
        if cli_args.help and not (cli_args.bg or cli_args.task):
            # Create a temporary parser just for help display
            from argparse import ArgumentParser

            help_parser = ArgumentParser(
                prog="clud",
                description="Launch a Claude-powered development container",
                formatter_class=ArgumentParser().formatter_class,
            )
            help_parser.print_help()
            return 0

        # Handle login command first (doesn't need Docker)
        if cli_args.login:
            return handle_login()

        # Handle task command (doesn't need Docker)
        if cli_args.task:
            return handle_task_command(cli_args.task)

        # Handle lint command (doesn't need Docker)
        if cli_args.lint:
            return handle_lint_command()

        # Handle test command (doesn't need Docker)
        if cli_args.test:
            return handle_test_command()

        # Handle fix command (doesn't need Docker)
        if cli_args.fix:
            return handle_fix_command()

        # Handle Git worktree commands (need Docker)
        worktree_commands = [cli_args.worktree_create, cli_args.worktree_new, cli_args.worktree_remove, cli_args.worktree_list, cli_args.worktree_prune, cli_args.worktree_cleanup]
        if any(worktree_commands):
            # Check Docker availability for worktree operations
            if not check_docker_available():
                raise DockerError("Docker is not available or not running")

            # Validate project path
            project_path = validate_path(cli_args.path)

            # Handle each worktree command
            if cli_args.worktree_create:
                success = create_git_worktree_in_container(project_path, cli_args.worktree_create, cli_args.worktree_name)
                return 0 if success else 1

            elif cli_args.worktree_new:
                success = create_git_worktree_in_container(project_path, cli_args.worktree_new, cli_args.worktree_name, create_new_branch=True)
                return 0 if success else 1

            elif cli_args.worktree_remove:
                success = remove_git_worktree_in_container(project_path, cli_args.worktree_name)
                return 0 if success else 1

            elif cli_args.worktree_list:
                result = list_git_worktrees_in_container(project_path)
                if result is not None:
                    print("Git worktrees:")
                    print(result)
                    return 0
                else:
                    return 1

            elif cli_args.worktree_prune:
                success = prune_git_worktrees_in_container(project_path)
                return 0 if success else 1

            elif cli_args.worktree_cleanup:
                success = cleanup_git_worktree(project_path, cli_args.worktree_name)
                return 0 if success else 1

        # Handle background shell mode
        if cli_args.bg:
            # Background shell mode - launch interactive bash shell with enforced entrypoint
            # Check Docker availability first
            if not check_docker_available():
                raise DockerError("Docker is not available or not running")

            # Import TTY validation here to avoid circular imports
            from .agent_background import validate_tty_for_interactive_mode

            # Validate TTY availability for interactive mode (unless specific command is provided)
            if not cli_args.cmd or cli_args.cmd == "/bin/bash":
                validate_tty_for_interactive_mode()

            # Use provided cmd or default to /bin/bash for workspace interaction
            if not cli_args.cmd:
                cli_args.cmd = "/bin/bash"

            # Build image if needed
            if not cli_args._image_built and not build_docker_image(cli_args.build_dockerfile):
                return 1

            # Get API key and launch container with enforced entrypoint
            api_key = get_api_key_from_cli_args(cli_args)
            # Convert to BackgroundAgentArgs
            bg_args = convert_to_background_args(cli_args)
            return launch_container_shell(bg_args, api_key)

        # Check if this is yolo mode (only if no Docker flags AND no path provided, OR if prompt is specified)
        # If a path is provided, default to Docker shell mode unless prompt is specified
        # Special case: if auto-build was triggered but no explicit path was provided, prefer yolo mode
        explicit_docker_mode = cli_args.ui or cli_args.update or cli_args.just_build or cli_args.bg
        explicit_build_requested = cli_args._original_build  # Track if build was explicitly requested

        is_yolo_mode = (not explicit_docker_mode and not explicit_build_requested and not cli_args.path) or cli_args.prompt

        if is_yolo_mode:
            # Handle yolo mode (doesn't need Docker)
            from .agent_foreground import main as yolo_main

            # Construct arguments for yolo main
            yolo_args: list[str] = []
            if cli_args.prompt:
                yolo_args.extend(["-p", cli_args.prompt])
            if cli_args.message:
                yolo_args.extend(["-m", cli_args.message])
            if cli_args.dry_run:
                yolo_args.append("--dry-run")

            return yolo_main(yolo_args)

        # From here on, we're in Docker-based mode
        # Check Docker availability first for all Docker-based modes
        if not check_docker_available():
            raise DockerError("Docker is not available or not running")

        # Handle update mode - always pull from remote registry
        if cli_args.update:
            print("Updating clud runtime...")

            # Determine which image to pull
            # User specified a custom image, otherwise use the standard Claude Code image from Docker Hub
            # This ensures we always pull from remote, not build locally
            image_to_pull = cli_args.image or "niteris/clud:latest"

            print(f"Pulling the latest version of {image_to_pull}...")

            if pull_latest_image(image_to_pull):
                print(f"Successfully updated {image_to_pull}")
                print("You can now run 'clud' to use the updated runtime.")

                # If they pulled a non-default image, remind them to use --image flag
                if image_to_pull != "niteris/clud:latest" and not cli_args.image:
                    print(f"Note: To use this image, run: clud --image {image_to_pull}")

                return 0
            else:
                print(f"Failed to update {image_to_pull}", file=sys.stderr)
                print("Please check your internet connection and Docker configuration.")
                return 1

        # Handle build-only mode
        if cli_args.just_build:
            force_rebuild = cli_args.build == "force"
            if force_rebuild:
                print("Force building Docker image (no cache)...")
            else:
                print("Building Docker image...")
            if build_docker_image(cli_args.build_dockerfile, force_rebuild=force_rebuild, skip_existing_check=True):
                print("Docker image built successfully!")
                return 0
            else:
                print("Failed to build Docker image", file=sys.stderr)
                return 1

        # Force build if requested
        if cli_args.build:
            force_rebuild = cli_args.build == "force"
            if force_rebuild:
                print("Force building Docker image (no cache)...")
            else:
                print("Building Docker image...")
            if not build_docker_image(cli_args.build_dockerfile, force_rebuild=force_rebuild, skip_existing_check=True):
                print("Failed to build Docker image", file=sys.stderr)
                return 1
            cli_args._image_built = True

        # Route to Docker-based modes
        if cli_args.ui:
            # UI mode - launch code-server container
            project_path = validate_path(cli_args.path)
            api_key = get_api_key_from_cli_args(cli_args)

            # Convert to BackgroundAgentArgs
            bg_args = convert_to_background_args(cli_args)
            return run_ui_container(bg_args, project_path, api_key)
        else:
            # Container shell mode - launch container with interactive shell
            api_key = get_api_key_from_cli_args(cli_args)
            # Convert to BackgroundAgentArgs
            bg_args = convert_to_background_args(cli_args)
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
