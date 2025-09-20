#!/usr/bin/env python3
"""Background agent for continuous workspace synchronization and Docker container management."""

import asyncio
import json
import logging
import os
import platform
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import traceback
import webbrowser
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from .agent_background_args import BackgroundAgentArgs, parse_background_agent_args
from .agent_completion import detect_agent_completion
from .running_process import RunningProcess

# Container sync is now handled by standalone package in container


# Exception classes
class CludError(Exception):
    """Base exception for clud errors."""

    pass


class ValidationError(CludError):
    """User/validation error."""

    pass


class DockerError(CludError):
    """Docker/runtime error."""

    pass


class ConfigError(CludError):
    """Configuration error."""

    pass


# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format="[%(asctime)s] [%(levelname)s] [bg-agent] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger(__name__)


def is_tty_available() -> bool:
    """Check if a TTY is available for interactive operations."""
    try:
        # Primary check: stdin must be a TTY
        # Secondary check: On Windows MSYS/MINGW, if stdin is not a TTY,
        # we still don't have proper TTY support even with winpty
        # winpty only helps with Windows console apps, not with piped input
        return hasattr(sys.stdin, "isatty") and sys.stdin.isatty()
    except (AttributeError, OSError):
        return False


def validate_tty_for_interactive_mode() -> None:
    """Validate that a TTY is available for interactive Docker containers.

    Raises:
        ValidationError: If no TTY is available for interactive operations
    """
    if not is_tty_available():
        raise ValidationError(
            "Interactive container mode (--bg) requires a TTY but none is available.\n"
            "This usually happens when:\n"
            "  - Running in a non-interactive environment (CI/CD, scripts)\n"
            "  - Output is redirected to a file or pipe\n"
            "  - Running in an IDE or editor that doesn't provide TTY access\n"
            "\n"
            "Solutions:\n"
            "  - Run from a proper terminal/shell\n"
            "  - Use specific commands with --cmd flag for non-interactive execution\n"
            "  - Remove --bg flag for non-interactive use cases"
        )


def dump_thread_stacks():
    """Dump stack traces of all running threads."""
    # Write to stderr to ensure it's visible even with subprocess redirects
    print("\n" + "=" * 50, file=sys.stderr, flush=True)
    print("THREAD DUMP EXECUTED", file=sys.stderr, flush=True)
    print("=" * 50, file=sys.stderr, flush=True)

    # Get all thread frames
    thread_frames = sys._current_frames()

    for thread_id, frame in thread_frames.items():
        # Get thread object by ID
        thread_obj = None
        for t in threading.enumerate():
            if t.ident == thread_id:
                thread_obj = t
                break

        thread_name = thread_obj.name if thread_obj else f"Thread-{thread_id}"
        daemon_status = " (daemon)" if thread_obj and thread_obj.daemon else ""

        print(f"\nThread: {thread_name} (ID: {thread_id}){daemon_status}", file=sys.stderr, flush=True)
        print("-" * 40, file=sys.stderr, flush=True)

        # Print the stack trace for this thread
        traceback.print_stack(frame, file=sys.stderr)

    print("\n" + "=" * 50, file=sys.stderr, flush=True)
    print("END THREAD DUMP", file=sys.stderr, flush=True)
    print("=" * 50 + "\n", file=sys.stderr, flush=True)

    # Also write to a file for verification
    try:
        with open("thread_dump_debug.txt", "w") as f:
            f.write(f"Thread dump executed at {time.time()}\n")
            f.write(f"Active threads: {len(thread_frames)}\n")
    except Exception:
        pass  # Don't fail if we can't write file


# Docker utility functions
def validate_path(path_str: str | None) -> Path:
    """Validate and resolve the project path."""
    if not path_str:
        path_str = os.getcwd()

    try:
        path = Path(path_str).resolve()
        if not path.exists():
            raise ValidationError(f"Directory does not exist: {path}")
        if not path.is_dir():
            raise ValidationError(f"Path is not a directory: {path}")
        return path
    except OSError as e:
        raise ValidationError(f"Invalid path '{path_str}': {e}") from e


def normalize_path_for_docker(path: Path) -> str:
    """Normalize path for Docker mounting, handling Windows paths."""
    if platform.system() == "Windows":
        # Convert Windows path to forward slash format for Docker Desktop
        # Keep the drive letter format: C:\path -> C:/path
        path_str = str(path).replace("\\", "/")
        return path_str
    return str(path)


def check_docker_available() -> bool:
    """Check if Docker is available and running."""
    try:
        subprocess.run(["docker", "version"], capture_output=True, check=True, timeout=10)
        return True
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        return False


def find_run_claude_docker() -> str | None:
    """Find run-claude-docker wrapper in PATH."""
    return shutil.which("run-claude-docker")


def get_ssh_dir() -> Path | None:
    """Get SSH directory path if it exists."""
    ssh_dir = Path.home() / ".ssh"
    return ssh_dir if ssh_dir.exists() and ssh_dir.is_dir() else None


def get_claude_commands_mount(claude_commands_path: str | None) -> tuple[str, str] | None:
    """Get Claude commands mount info if path is provided and valid."""
    if not claude_commands_path:
        return None

    try:
        path = Path(claude_commands_path).resolve()
        if not path.exists():
            raise ValidationError(f"Claude commands path does not exist: {path}")

        docker_path = normalize_path_for_docker(path)

        if path.is_file():
            # Single file - mount to appropriate filename in plugins directory
            filename = path.name
            if not filename.endswith(".md"):
                raise ValidationError(f"Claude command file must be a .md file: {filename}")
            return (docker_path, f"/plugins/{filename}")
        elif path.is_dir():
            # Directory - mount entire directory to plugins
            return (docker_path, "/plugins")
        else:
            raise ValidationError(f"Claude commands path is neither file nor directory: {path}")

    except OSError as e:
        raise ValidationError(f"Invalid claude commands path '{claude_commands_path}': {e}") from e


def get_worktree_mount(project_path: Path, worktree_name: str = "worktree") -> tuple[str, str]:
    """DEPRECATED: Get worktree mount info - now returns project path since worktrees are container-only.

    Args:
        project_path: The project root directory containing .git
        worktree_name: Name of the worktree subdirectory - DEPRECATED, not used

    Returns:
        Tuple of (host_path, container_path) - returns project mount info

    Raises:
        ValidationError: If the project directory is invalid
    """
    # Validate that project_path exists
    if not project_path.exists():
        raise ValidationError(f"Project directory does not exist: {project_path}")

    docker_path = normalize_path_for_docker(project_path)
    return (docker_path, "/host")  # Return host mount point instead


def is_port_available(port: int) -> bool:
    """Check if a port is available for binding."""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.bind(("localhost", port))
            return True
    except OSError:
        return False


def find_available_port(start_port: int = 8743) -> int:
    """Find an available port starting from start_port."""
    for port in range(start_port, start_port + 100):
        if is_port_available(port):
            return port
    raise DockerError(f"No available ports found starting from {start_port}")


def load_clud_config() -> dict[str, Any] | None:
    """Load .clud configuration file if it exists."""
    clud_config_path = Path.cwd() / ".clud"
    if clud_config_path.exists():
        try:
            with open(clud_config_path, encoding="utf-8") as f:
                config = json.load(f)
                return config
        except (OSError, json.JSONDecodeError) as e:
            print(f"Warning: Failed to parse .clud config file: {e}")
    return None


def pull_latest_image(image_name: str = "niteris/clud:latest") -> bool:
    """Pull the latest version of a Docker image."""
    try:
        print(f"Pulling the latest version of {image_name}...")

        # Use docker pull command to get the latest image
        cmd = ["docker", "pull", image_name]
        result = subprocess.run(cmd, capture_output=False, text=True)

        if result.returncode == 0:
            print(f"Successfully pulled the latest version of {image_name}")
            return True
        else:
            print(f"Failed to pull {image_name}", file=sys.stderr)
            return False

    except FileNotFoundError as err:
        raise DockerError("Docker command not found. Make sure Docker is installed.") from err
    except Exception as e:
        print(f"Error pulling image: {e}", file=sys.stderr)
        return False


def build_docker_image(dockerfile_path: str | None = None, force_rebuild: bool = False, skip_existing_check: bool = False) -> bool:
    """Build the niteris/clud Docker image if it doesn't exist."""
    try:
        # Check if image already exists (skip this check if force_rebuild or skip_existing_check is True)
        if not force_rebuild and not skip_existing_check:
            result = subprocess.run(["docker", "images", "-q", "niteris/clud:latest"], capture_output=True, text=True, check=True, timeout=30, encoding="utf-8", errors="replace")

            if result.stdout.strip():
                print("Docker image niteris/clud:latest already exists")
                return True

        print("Building niteris/clud Docker image...")

        # Determine dockerfile to use (priority order)
        if dockerfile_path:
            # Priority 1: Use custom dockerfile path from command line
            dockerfile = Path(dockerfile_path)
            if not dockerfile.exists():
                raise DockerError(f"Custom dockerfile not found: {dockerfile_path}")
            build_context = dockerfile.parent
            cmd = ["docker", "build", "-t", "niteris/clud:latest", "-f", str(dockerfile), str(build_context)]
            print(f"Using custom dockerfile: {dockerfile_path}")
        else:
            # Priority 2: Check for .clud config file
            config = load_clud_config()
            if config and "dockerfile" in config:
                # Use dockerfile path from .clud config
                config_dockerfile_path: str = config["dockerfile"]
                dockerfile = Path(config_dockerfile_path)
                if not dockerfile.exists():
                    raise DockerError(f"Dockerfile specified in .clud config not found: {config_dockerfile_path}")
                build_context = dockerfile.parent
                cmd = ["docker", "build", "-t", "niteris/clud:latest", "-f", str(dockerfile), str(build_context)]
                print(f"INFO: Using dockerfile from .clud config: {config_dockerfile_path}")
            elif (Path.cwd() / "Dockerfile").exists():
                # Priority 3: Use local Dockerfile in current directory
                cmd = ["docker", "build", "-t", "niteris/clud:latest", "."]
                print("Using local Dockerfile from current directory")
            else:
                # Priority 4: Fallback to remote image - don't build locally
                print("No local Dockerfile found")
                print("Using remote image instead of building locally")
                return True

        # Add --no-cache flag if force_rebuild is True
        if force_rebuild:
            cmd.insert(-1, "--no-cache")

        # Build the image
        result = subprocess.run(cmd, check=True)

        print("Docker image built successfully")
        return True

    except subprocess.CalledProcessError as e:
        print(f"Failed to build Docker image: {e}")
        return False
    except FileNotFoundError as err:
        raise DockerError("Docker command not found. Make sure Docker is installed.") from err


def stop_existing_container(container_name: str = "clud-dev") -> None:
    """Stop and remove existing container if it exists."""
    try:
        # Check if container exists and is running
        result = subprocess.run(["docker", "ps", "-q", "-f", f"name={container_name}"], capture_output=True, text=True, check=True, timeout=30, encoding="utf-8", errors="replace")

        if result.stdout.strip():
            print(f"Stopping existing container {container_name}...")
            subprocess.run(["docker", "stop", container_name], check=True, capture_output=True, timeout=60)

        # Check if container exists (stopped)
        result = subprocess.run(["docker", "ps", "-aq", "-f", f"name={container_name}"], capture_output=True, text=True, check=True, timeout=30, encoding="utf-8", errors="replace")

        if result.stdout.strip():
            print(f"Removing existing container {container_name}...")
            subprocess.run(["docker", "rm", container_name], check=True, capture_output=True, timeout=60)

    except subprocess.CalledProcessError:
        # Container might not exist, which is fine
        pass


# Container management functions
def run_ui_container(args: BackgroundAgentArgs, project_path: Path, api_key: str) -> int:
    """Run the code-server UI container."""
    # Find available port
    port = args.port
    if not is_port_available(port):
        print(f"Port {port} is not available, finding alternative...")
        port = find_available_port(port)
        print(f"Using port {port}")

    # Build image if not already built
    if not args._image_built and not build_docker_image(args.build_dockerfile):
        return 1

    # Stop existing container
    stop_existing_container()

    # Prepare Docker command - use foreground mode for streaming
    docker_path = normalize_path_for_docker(project_path)
    # Note: Not mounting .local to preserve container's installed tools (Claude CLI, etc.)
    # Only mount .config for user settings
    home_config_path = normalize_path_for_docker(Path.home() / ".config")

    cmd = [
        "docker",
        "run",
        "--rm",  # Remove container when it stops
        "--name",
        "clud-dev",
        "-p",
        f"{port}:8080",
        "-e",
        f"ANTHROPIC_API_KEY={api_key}",
        "-e",
        "PASSWORD=",  # No authentication
        "-e",
        "CLUD_BACKGROUND_SYNC=true",  # Enable background sync
        "-e",
        "CLUD_SYNC_INTERVAL=10",  # 10 second sync interval
        "-v",
        f"{docker_path}:/host:rw",  # Mount to /host for sync
        "-v",
        f"{home_config_path}:/root/.config",
        # Removed .local mount to preserve container's installed CLI tools
    ]

    # Add Claude commands mount if specified
    claude_mount = get_claude_commands_mount(args.claude_commands)
    if claude_mount:
        host_path, container_path = claude_mount
        cmd.extend(["-v", f"{host_path}:{container_path}:ro"])
        print(f"Mounting Claude commands: {host_path} -> {container_path}")

    # Add worktree mount (always mount for potential worktree operations)
    worktree_mount = get_worktree_mount(project_path, args.worktree_name)
    host_path, container_path = worktree_mount
    cmd.extend(["-v", f"{host_path}:{container_path}:rw"])
    print(f"Mounting Git worktree: {host_path} -> {container_path}")

    cmd.append("niteris/clud:latest")

    print("Starting CLUD development container...")

    try:
        # Set up environment with API key
        env = os.environ.copy()
        env["ANTHROPIC_API_KEY"] = api_key

        # Schedule browser opening after a short delay
        def open_browser_delayed():
            time.sleep(3)  # Wait for server to start
            url = f"http://localhost:{port}"
            print(f"\nOpening browser to {url}")
            try:
                webbrowser.open(url)
                print(f"""
Code-server is now running!
- URL: {url}
- Container: clud-dev
- Project: {project_path}

Press Ctrl+C to stop the container
""")
            except Exception as e:
                print(f"Could not open browser automatically: {e}")
                print(f"Please open {url} in your browser")

        # Start browser opening in background thread
        browser_thread = threading.Thread(target=open_browser_delayed, daemon=True)
        browser_thread.start()

        # Use RunningProcess for streaming output
        try:
            return RunningProcess.run_streaming(cmd, env=env)
        except KeyboardInterrupt:
            print("\nTerminating container...", file=sys.stderr)
            return 130  # Standard exit code for SIGINT

    except Exception as e:
        print(f"Failed to start container: {e}")
        return 1


def build_wrapper_command(args: BackgroundAgentArgs, project_path: Path) -> list[str]:
    """Build command for run-claude-docker wrapper."""
    cmd = ["run-claude-docker"]

    # Always pass workspace
    cmd.extend(["--workspace", str(project_path)])

    # Map clud options to wrapper flags
    # Dangerous mode is default, only skip if --no-dangerous is specified
    if not args.no_dangerous:
        cmd.append("--dangerously-skip-permissions")

    if args.shell != "bash":
        cmd.extend(["--shell", args.shell])

    if args.image:
        cmd.extend(["--image", args.image])

    if args.profile != "python":
        cmd.extend(["--profile", args.profile])

    if args.no_firewall:
        cmd.append("--disable-firewall")

    # Sudo is enabled by default unless --no-sudo is specified
    if not args.no_sudo:
        cmd.append("--enable-sudo")

    return cmd


def build_fallback_command(args: BackgroundAgentArgs, project_path: Path) -> list[str]:
    """Build direct docker run command as fallback."""
    project_name = project_path.name
    docker_path = normalize_path_for_docker(project_path)

    cmd = ["docker", "run", "-it", "--rm", f"--name=clud-{project_name}", f"--volume={docker_path}:/host:rw"]

    # Add SSH keys mount if requested
    if args.ssh_keys:
        ssh_dir = get_ssh_dir()
        if not ssh_dir:
            raise ValidationError("SSH directory ~/.ssh not found")
        ssh_docker_path = normalize_path_for_docker(ssh_dir)
        cmd.append(f"--volume={ssh_docker_path}:/home/dev/.ssh:ro")

    # Add read-only home mount if requested
    if args.read_only_home:
        home_docker_path = normalize_path_for_docker(Path.home())
        cmd.append(f"--volume={home_docker_path}:/host-home:ro")

    # Add Claude commands mount if specified
    claude_mount = get_claude_commands_mount(args.claude_commands)
    if claude_mount:
        host_path, container_path = claude_mount
        cmd.append(f"--volume={host_path}:{container_path}:ro")
        print(f"Mounting Claude commands: {host_path} -> {container_path}")

    # Network settings
    if args.no_firewall:
        cmd.append("--network=none")

    # User and sudo settings
    if args.no_sudo and platform.system() != "Windows":
        uid = os.getuid()
        gid = os.getgid()
        cmd.extend(["--user", f"{uid}:{gid}"])

    # Environment variables
    env_vars: list[str] = args.env or []

    # Add ANTHROPIC_API_KEY if available in environment
    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if api_key:
        cmd.extend(["-e", f"ANTHROPIC_API_KEY={api_key}"])

    # Add custom environment variables
    for env_var in env_vars:
        if "=" not in env_var:
            raise ValidationError(f"Invalid environment variable format: {env_var}")
        cmd.extend(["-e", env_var])

    # Use default image if not specified
    image = args.image or "niteris/clud:latest"
    cmd.append(image)

    # Default entrypoint: launch claude in workspace
    claude_cmd = ["claude", "code"]
    # Dangerous mode is default, only skip if --no-dangerous is specified
    if not args.no_dangerous:
        claude_cmd.append("--dangerously-skip-permissions")

    cmd.extend(claude_cmd)
    return cmd


def run_container(args: BackgroundAgentArgs, api_key: str) -> int:
    """Main logic to run the container."""
    # Validate project path
    project_path = validate_path(args.path)

    # Temporarily set the API key in environment for subprocess
    env = os.environ.copy()
    env["ANTHROPIC_API_KEY"] = api_key

    # Docker availability already checked in main()

    # Try wrapper first, then fallback
    wrapper_path = find_run_claude_docker()

    if wrapper_path:
        cmd = build_wrapper_command(args, project_path)
        print(f"Using run-claude-docker wrapper: {' '.join(cmd)}")
    else:
        cmd = build_fallback_command(args, project_path)
        print(f"Using direct docker run: {' '.join(cmd[:5])}...")  # Truncate for readability

    # Execute the command with the API key in environment
    try:
        # Use subprocess.run with check=False to propagate exit codes
        result = subprocess.run(cmd, env=env, check=False)
        return result.returncode
    except FileNotFoundError as e:
        raise DockerError(f"Command not found: {cmd[0]}") from e
    except Exception as e:
        raise DockerError(f"Failed to run container: {e}") from e


def launch_container_shell(args: BackgroundAgentArgs, api_key: str) -> int:
    """Launch container with enforced entrypoint and user-specified command."""
    # Validate project path
    project_path = validate_path(args.path)

    # Docker availability already checked in main()

    # Build image if not already built
    if not args._image_built and not build_docker_image(args.build_dockerfile):
        return 1

    # Stop existing container
    stop_existing_container()

    # Prepare Docker command
    docker_path = normalize_path_for_docker(project_path)

    # Always use the standard container entrypoint - NEVER override it
    # This ensures consistent behavior and security
    # Determine if we need TTY allocation based on command type and availability
    is_interactive = not args.cmd or args.cmd == "/bin/bash"
    has_tty = is_tty_available()

    base_cmd = ["docker", "run"]

    # Add TTY and interactive flags based on context
    if is_interactive and has_tty:
        # Interactive shell with TTY - use both -i and -t
        base_cmd.extend(["-it"])
    elif is_interactive and not has_tty:
        # Interactive shell without TTY - use only -i for stdin
        base_cmd.extend(["-i"])
        logger.warning("Running in interactive mode without TTY - some features may not work properly")
    elif not is_interactive and has_tty:
        # Non-interactive command with TTY available - allocate TTY for better output formatting
        base_cmd.extend(["-t"])
    # else: Non-interactive command without TTY - no additional flags needed

    base_cmd.extend(
        [
            "--rm",
            "--name",
            "clud-dev",
            "-e",
            f"ANTHROPIC_API_KEY={api_key}",
            "-e",
            "CLUD_BACKGROUND_SYNC=true",  # Enable background sync
            "-e",
            "CLUD_SYNC_INTERVAL=300",  # 5 minute sync interval
            "-v",
            f"{docker_path}:/host:rw",
            "-w",
            "/workspace",  # Set working directory to /workspace
        ]
    )

    # If --open flag is set, expose the code-server port and enable code-server
    if args.open:
        # Find available port
        port = args.port
        if not is_port_available(port):
            print(f"Port {port} is not available, finding alternative...")
            port = find_available_port(port)
            print(f"Using port {port}")

        # Add port mapping and environment variable to enable code-server
        base_cmd.extend(
            [
                "-p",
                f"{port}:8080",
                "-e",
                "PASSWORD=",  # No authentication for code-server
            ]
        )

        # Schedule browser opening after container starts
        def open_browser_delayed():
            time.sleep(5)  # Wait for code-server to start (increased delay)
            url = f"http://localhost:{port}"
            print(f"\nOpening browser to VS Code server at {url}")
            try:
                webbrowser.open(url)
                print(f"VS Code server is now accessible at {url}")
                print("Note: If the page doesn't load, wait a few more seconds and refresh")
            except Exception as e:
                print(f"Could not open browser automatically: {e}")
                print(f"Please open {url} in your browser")

        # Start browser opening in background thread
        browser_thread = threading.Thread(target=open_browser_delayed, daemon=True)
        browser_thread.start()

    # Add Claude commands mount if specified
    claude_mount = get_claude_commands_mount(args.claude_commands)
    if claude_mount:
        host_path, container_path = claude_mount
        base_cmd.extend(["-v", f"{host_path}:{container_path}:ro"])
        print(f"Mounting Claude commands: {host_path} -> {container_path}")

    # Add worktree mount if it exists
    worktree_mount = get_worktree_mount(project_path, args.worktree_name)
    if worktree_mount:
        host_path, container_path = worktree_mount
        base_cmd.extend(["-v", f"{host_path}:{container_path}:rw"])
        print(f"Mounting Git worktree: {host_path} -> {container_path}")

    # Add the image
    base_cmd.append("niteris/clud:latest")

    # Add the command - this is the ONLY part that can be customized
    # Default to bash if no command specified, otherwise use the provided command
    if args.cmd:
        # ALL commands must use --cmd format to go through entrypoint.sh properly
        if args.cmd == "/bin/bash":
            # Special case for interactive bash
            if args.open:
                # When --open is used, start code-server in background then run bash
                cmd_string = "(code-server --bind-addr=0.0.0.0:8080 --auth=none --disable-telemetry /workspace &) && /bin/bash --login"
                base_cmd.extend(["--cmd", cmd_string])
            else:
                # Normal interactive bash without code-server
                base_cmd.extend(["--cmd", "/bin/bash --login"])
        else:
            # For other commands, use --cmd format so entrypoint.sh handles them
            base_cmd.extend(["--cmd", args.cmd])
    else:
        # Default interactive shell
        if args.open:
            # When --open is used, start code-server in background then run bash
            cmd_string = "(code-server --bind-addr=0.0.0.0:8080 --auth=none --disable-telemetry /workspace &) && /bin/bash --login"
            base_cmd.extend(["--cmd", cmd_string])
        else:
            base_cmd.extend(["/bin/bash", "--login"])

    # On Windows with mintty/git-bash, prepend winpty for TTY support when needed
    msystem = os.environ.get("MSYSTEM", "")
    needs_winpty = (
        platform.system() == "Windows"
        and msystem.startswith(("MSYS", "MINGW"))
        and is_interactive  # Only use winpty for interactive sessions
        and has_tty  # Only when TTY is available
    )

    if needs_winpty:
        # Check if winpty is available
        if shutil.which("winpty"):
            cmd = ["winpty"] + base_cmd
            logger.debug("Using winpty for Windows TTY support")
        else:
            cmd = base_cmd
            logger.warning("winpty not found - terminal features may be limited on Windows")
    else:
        cmd = base_cmd

    print("Starting CLUD development container...")

    # Schedule thread dump if requested - run in independent thread
    dump_thread = None
    dump_seconds = args.dump_threads_after
    if dump_seconds is not None:
        print(f"Thread dump scheduled in {dump_seconds} seconds...")

        def independent_dump_timer():
            """Independent thread that waits and dumps, not affected by main thread blocking."""
            try:
                # Debug: write start message
                with open("dump_timer_debug.txt", "w") as f:
                    f.write(f"Timer thread started, sleeping for {dump_seconds} seconds\n")

                time.sleep(dump_seconds)

                # Debug: write execution message
                with open("dump_timer_debug.txt", "a") as f:
                    f.write(f"Timer fired, executing dump at {time.time()}\n")

                dump_thread_stacks()

                # Debug: write completion message
                with open("dump_timer_debug.txt", "a") as f:
                    f.write("Dump completed\n")
            except Exception as e:
                # Debug: write error message
                with open("dump_timer_error.txt", "w") as f:
                    f.write(f"Timer thread error: {e}\n")
                    import traceback

                    traceback.print_exc(file=f)

        dump_thread = threading.Thread(target=independent_dump_timer, daemon=True)
        dump_thread.start()

    try:
        # Set up environment with API key
        env = os.environ.copy()
        env["ANTHROPIC_API_KEY"] = api_key

        # Check if completion detection is enabled (for automated workflows)
        if args.detect_completion:
            # Use agent completion detection
            idle_timeout = args.idle_timeout
            detect_agent_completion(cmd, idle_timeout)
            return 0
        else:
            # Use subprocess.run for direct terminal passthrough
            # Set up proper signal handling for PTY processes
            try:
                if is_interactive and has_tty:
                    # For interactive TTY sessions, ensure proper signal propagation
                    result = subprocess.run(cmd, env=env, check=False, preexec_fn=os.setsid) if platform.system() != "Windows" else subprocess.run(cmd, env=env, check=False)
                else:
                    # For non-interactive sessions, use standard subprocess
                    result = subprocess.run(cmd, env=env, check=False)

                return result.returncode
            except OSError as e:
                # Handle cases where process group creation fails
                logger.warning(f"Failed to create process group: {e}, falling back to standard subprocess")
                result = subprocess.run(cmd, env=env, check=False)
                return result.returncode

    except KeyboardInterrupt:
        print("\nContainer terminated.", file=sys.stderr)
        return 130
    except Exception as e:
        raise DockerError(f"Failed to start container shell: {e}") from e
    finally:
        # Note: dump_thread runs independently and will terminate naturally
        # No need to cancel it since it's a daemon thread with time.sleep()
        pass


class BackgroundAgent:
    """Background agent for managing workspace synchronization via containers."""

    def __init__(
        self,
        host_dir: str,
        workspace_dir: str,
        sync_interval: int = 300,
        watch_mode: bool = False,
    ):
        self.host_dir = Path(host_dir)
        self.workspace_dir = Path(workspace_dir)
        self.sync_interval = sync_interval  # seconds
        self.watch_mode = watch_mode
        self.running = False
        self.last_sync_time: datetime | None = None
        self.sync_count = 0
        self.error_count = 0
        self.last_error: str | None = None
        # Set state file path based on platform
        if platform.system() == "Windows":
            self.state_file = Path(os.environ.get("TEMP", "C:/temp")) / "clud-bg-agent.state"
        else:
            self.state_file = Path("/var/run/clud-bg-agent.state")

        # Set up signal handlers
        signal.signal(signal.SIGTERM, self._handle_signal)

        # On Windows, be more careful with SIGINT handling to avoid spurious signals
        if platform.system() == "Windows":
            # Only handle SIGINT in specific Windows environments
            if os.environ.get("MSYSTEM", "").startswith(("MSYS", "MINGW")):
                signal.signal(signal.SIGINT, self._handle_signal)
            else:
                logger.debug("Skipping SIGINT handler on Windows (not in MSYS/MINGW environment)")
        else:
            signal.signal(signal.SIGINT, self._handle_signal)

    def install_claude_plugins(self) -> bool:
        """Install Claude plugins from workspace to system.

        NOTE: This function is currently disabled. Plugins should be volume-mapped
        to /plugins in the container and then processed separately.
        """
        logger.info("Plugin installation via rsync is disabled - using volume mapping instead")
        return True

    def _handle_signal(self, signum: int, frame: Any) -> None:
        """Handle shutdown signals gracefully."""
        logger.info(f"Received signal {signum}, shutting down...")
        self.running = False

    def run_container_sync(self, command: str) -> bool:
        """Run sync command in container."""
        try:
            docker_path = normalize_path_for_docker(self.host_dir)
            cmd = [
                "docker",
                "run",
                "--rm",
                "-v",
                f"{docker_path}:/host:rw",
                "-v",
                f"{docker_path}/workspace:/workspace:rw",
                "niteris/clud:latest",
                "python",
                "/opt/container_sync/container_sync.py",
                command,
                "--host-dir",
                "/host",
                "--workspace-dir",
                "/workspace",
            ]

            # Show banner message before entering Docker
            logger.info("### ENTERING DOCKER ###")

            # Custom callbacks for logging container output
            def stdout_callback(line: str) -> None:
                logger.info(f"[DOCKER] {line.rstrip()}")

            def stderr_callback(line: str) -> None:
                logger.warning(f"[DOCKER] {line.rstrip()}")

            # Use RunningProcess for streaming output
            returncode = RunningProcess.run_streaming(cmd, stdout_callback=stdout_callback, stderr_callback=stderr_callback)

            return returncode == 0

        except Exception as e:
            logger.error(f"Container sync failed: {e}")
            return False

    def initial_sync(self) -> bool:
        """Perform initial host → workspace sync via container."""
        logger.info("Performing initial sync from host to workspace...")

        # Create workspace directory if it doesn't exist
        self.workspace_dir.mkdir(parents=True, exist_ok=True)

        success = self.run_container_sync("init")
        if success:
            self.last_sync_time = datetime.now()
            self.sync_count += 1
            logger.info("Initial sync completed successfully")
            return True
        else:
            logger.error("Initial sync failed")
            self.error_count += 1
            self.last_error = "Initial sync failed"
            return False

    def bidirectional_sync(self) -> bool:
        """Perform bidirectional sync between host and workspace via container."""
        logger.info("Starting bidirectional sync...")

        success = self.run_container_sync("sync")
        if success:
            self.last_sync_time = datetime.now()
            self.sync_count += 1
            logger.info(f"Bidirectional sync completed (total syncs: {self.sync_count})")
            return True
        else:
            logger.warning("Bidirectional sync failed")
            self.error_count += 1
            self.last_error = "Bidirectional sync failed"
            return False

    def write_state(self):
        """Write agent state to file for monitoring."""
        try:
            state = {
                "running": self.running,
                "last_sync": self.last_sync_time.isoformat() if self.last_sync_time else None,
                "sync_count": self.sync_count,
                "error_count": self.error_count,
                "last_error": self.last_error,
                "sync_interval": self.sync_interval,
                "watch_mode": self.watch_mode,
            }

            # Write state as simple key=value pairs
            self.state_file.parent.mkdir(parents=True, exist_ok=True)
            with open(self.state_file, "w") as f:
                for key, value in state.items():
                    f.write(f"{key}={value}\n")

        except Exception as e:
            logger.warning(f"Failed to write state file: {e}")

    async def schedule_periodic_sync(self):
        """Background sync task that runs periodically."""
        logger.info(f"Starting periodic sync scheduler (interval: {self.sync_interval}s)")
        self.running = True

        # Perform initial sync
        if not self.initial_sync():
            logger.warning("Initial sync failed, continuing with periodic sync...")

        # Install Claude plugins after initial sync
        logger.info("Installing Claude plugins...")
        if self.install_claude_plugins():
            logger.info("Claude plugins installed successfully")
        else:
            logger.warning("Failed to install Claude plugins")

        while self.running:
            try:
                # Wait for the sync interval
                await asyncio.sleep(self.sync_interval)

                if not self.running:
                    break

                # Perform bidirectional sync
                logger.info(f"Triggering scheduled sync (#{self.sync_count + 1})")
                self.bidirectional_sync()

                # Write state file
                self.write_state()

            except asyncio.CancelledError:
                logger.info("Periodic sync cancelled")
                break
            except Exception as e:
                logger.error(f"Error in periodic sync loop: {e}")
                self.error_count += 1
                self.last_error = str(e)
                await asyncio.sleep(10)  # Brief pause before retrying

        logger.info("Periodic sync scheduler stopped")

    async def watch_for_changes(self):
        """File system watcher for auto-sync (placeholder for future implementation)."""
        if not self.watch_mode:
            return

        logger.info("File watcher mode is not yet implemented")
        # Future implementation could use:
        # - inotify on Linux
        # - watchdog library for cross-platform support
        # - Polling for simple implementation

    def run(self):
        """Main entry point for the background agent."""
        logger.info("=== CLUD Background Sync Agent Starting ===")
        logger.info(f"Host directory: {self.host_dir}")
        logger.info(f"Workspace directory: {self.workspace_dir}")
        logger.info(f"Sync interval: {self.sync_interval}s")
        logger.info(f"Watch mode: {self.watch_mode}")

        try:
            # Create event loop and run
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)

            # Schedule tasks
            tasks = [loop.create_task(self.schedule_periodic_sync())]

            if self.watch_mode:
                tasks.append(loop.create_task(self.watch_for_changes()))

            # Run until interrupted
            loop.run_until_complete(asyncio.gather(*tasks))

        except KeyboardInterrupt:
            logger.info("Received keyboard interrupt")
        except Exception as e:
            logger.error(f"Fatal error in background agent: {e}")
            sys.exit(1)
        finally:
            # Clean shutdown
            self.running = False
            self.write_state()
            logger.info("Background agent stopped")

    def status(self) -> dict[str, Any]:
        """Get current agent status."""
        return {
            "running": self.running,
            "last_sync": self.last_sync_time,
            "sync_count": self.sync_count,
            "error_count": self.error_count,
            "last_error": self.last_error,
            "next_sync": (self.last_sync_time + timedelta(seconds=self.sync_interval) if self.last_sync_time else None),
        }


def main(args: list[str] | None = None):
    """Main entry point for background agent."""
    parsed_args = parse_background_agent_args(args)

    if parsed_args.verbose:
        logger.setLevel(logging.DEBUG)
        # Also set container_sync logger to debug
        logging.getLogger("clud.container_sync").setLevel(logging.DEBUG)

    # Validate sync interval
    if parsed_args.sync_interval < 10:
        logger.error("Sync interval must be at least 10 seconds")
        sys.exit(1)

    if parsed_args.sync_interval > 3600:
        logger.warning("Large sync interval detected (> 1 hour), consider using a smaller interval")

    # We need to get the default workspace dir for comparison
    default_workspace_dir = "/workspace"
    in_container = Path("/.dockerenv").exists() or os.environ.get("CLUD_BACKGROUND_SYNC") == "true" or Path("/host").exists() or Path("/workspace").exists()
    if platform.system() == "Windows" and not in_container:
        default_workspace_dir = str(Path.cwd() / "workspace")

    # Validate that host and workspace directories are different
    host_path = Path(parsed_args.host_dir).resolve()
    workspace_path = Path(default_workspace_dir).resolve()

    if host_path == workspace_path:
        logger.error("Host directory and workspace directory cannot be the same")
        logger.error(f"Host: {host_path}")
        logger.error(f"Workspace: {workspace_path}")
        logger.error("Background sync requires different source and destination directories")
        sys.exit(1)

    # Create and run agent
    agent = BackgroundAgent(
        host_dir=parsed_args.host_dir,
        workspace_dir=default_workspace_dir,
        sync_interval=parsed_args.sync_interval,
        watch_mode=parsed_args.watch,
    )

    agent.run()


if __name__ == "__main__":
    main()
