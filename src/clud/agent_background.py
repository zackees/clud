#!/usr/bin/env python3
"""Background agent for continuous workspace synchronization and Docker container management."""

import argparse
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
import webbrowser
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from clud.container_sync import ContainerSync


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


def build_docker_image(dockerfile_path: str | None = None, force_rebuild: bool = False) -> bool:
    """Build the niteris/clud Docker image if it doesn't exist."""
    try:
        # Check if image already exists (skip this check if force_rebuild is True)
        if not force_rebuild:
            result = subprocess.run(["docker", "images", "-q", "niteris/clud:latest"], capture_output=True, text=True, check=True)

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
        result = subprocess.run(["docker", "ps", "-q", "-f", f"name={container_name}"], capture_output=True, text=True, check=True)

        if result.stdout.strip():
            print(f"Stopping existing container {container_name}...")
            subprocess.run(["docker", "stop", container_name], check=True, capture_output=True)

        # Check if container exists (stopped)
        result = subprocess.run(["docker", "ps", "-aq", "-f", f"name={container_name}"], capture_output=True, text=True, check=True)

        if result.stdout.strip():
            print(f"Removing existing container {container_name}...")
            subprocess.run(["docker", "rm", container_name], check=True, capture_output=True)

    except subprocess.CalledProcessError:
        # Container might not exist, which is fine
        pass


def stream_process_output(process: subprocess.Popen[str]) -> int:
    """Stream output from a subprocess in real-time."""
    try:
        # Stream stdout and stderr in real-time
        while True:
            # Check if process has terminated
            if process.poll() is not None:
                break

            # Read and print any available output
            if process.stdout:
                line = process.stdout.readline()
                if line:
                    print(line.rstrip(), flush=True)

            if process.stderr:
                line = process.stderr.readline()
                if line:
                    print(line.rstrip(), file=sys.stderr, flush=True)

            # Small delay to prevent busy waiting
            time.sleep(0.01)

        # Get any remaining output
        if process.stdout:
            for line in process.stdout:
                print(line.rstrip(), flush=True)

        if process.stderr:
            for line in process.stderr:
                print(line.rstrip(), file=sys.stderr, flush=True)

        # Wait for process to complete and return exit code
        return process.wait()

    except KeyboardInterrupt:
        print("\nTerminating container...", file=sys.stderr)
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        return 130  # Standard exit code for SIGINT


# Container management functions
def run_ui_container(args: argparse.Namespace, project_path: Path, api_key: str) -> int:
    """Run the code-server UI container."""
    # Find available port
    port = args.port
    if not is_port_available(port):
        print(f"Port {port} is not available, finding alternative...")
        port = find_available_port(port)
        print(f"Using port {port}")

    # Build image if not already built
    if (not hasattr(args, "_image_built") or not args._image_built) and not build_docker_image(getattr(args, "build_dockerfile", None)):
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
        f"{home_config_path}:/home/coder/.config",
        # Removed .local mount to preserve container's installed CLI tools
    ]

    # Add Claude commands mount if specified
    claude_mount = get_claude_commands_mount(getattr(args, "claude_commands", None))
    if claude_mount:
        host_path, container_path = claude_mount
        cmd.extend(["-v", f"{host_path}:{container_path}:ro"])
        print(f"Mounting Claude commands: {host_path} -> {container_path}")

    cmd.append("niteris/clud:latest")

    print("Starting CLUD development container...")

    try:
        # Set up environment with API key
        env = os.environ.copy()
        env["ANTHROPIC_API_KEY"] = api_key

        # Start process with streaming output
        process = subprocess.Popen(
            cmd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,  # Line buffered
            universal_newlines=True,
        )

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

        # Stream output in real-time
        return stream_process_output(process)

    except Exception as e:
        print(f"Failed to start container: {e}")
        return 1


def build_wrapper_command(args: argparse.Namespace, project_path: Path) -> list[str]:
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


def build_fallback_command(args: argparse.Namespace, project_path: Path) -> list[str]:
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
    claude_mount = get_claude_commands_mount(getattr(args, "claude_commands", None))
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


def run_container(args: argparse.Namespace, api_key: str) -> int:
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


def launch_container_shell(args: argparse.Namespace, api_key: str) -> int:
    """Launch container and drop user into bash shell at /workspace or execute specified command."""
    # Validate project path
    project_path = validate_path(args.path)

    # Docker availability already checked in main()

    # Build image if not already built
    if (not hasattr(args, "_image_built") or not args._image_built) and not build_docker_image(getattr(args, "build_dockerfile", None)):
        return 1

    # Stop existing container
    stop_existing_container()

    # Prepare Docker command
    docker_path = normalize_path_for_docker(project_path)

    # Determine if we're running a custom command or interactive shell
    if args.cmd:
        # Non-interactive mode for custom commands
        # Use the entrypoint.sh but pass the command as arguments
        cmd = [
            "docker",
            "run",
            "--rm",
            "--name",
            "clud-dev",
            "-e",
            f"ANTHROPIC_API_KEY={api_key}",
            "-e",
            "CLUD_BACKGROUND_SYNC=false",  # Disable background sync for command execution
            "-e",
            f"CLUD_CUSTOM_CMD={args.cmd}",  # Pass command via environment variable
            "-v",
            f"{docker_path}:/host:rw",
        ]

        # Add Claude commands mount if specified
        claude_mount = get_claude_commands_mount(getattr(args, "claude_commands", None))
        if claude_mount:
            host_path, container_path = claude_mount
            cmd.extend(["-v", f"{host_path}:{container_path}:ro"])
            print(f"Mounting Claude commands: {host_path} -> {container_path}")

        cmd.extend(
            [
                "niteris/clud:latest",
                "--cmd",
                args.cmd,  # Pass command as argument to entrypoint
            ]
        )
    else:
        # Interactive shell mode - override entrypoint to start bash with login shell
        base_cmd = [
            "docker",
            "run",
            "-it",
            "--rm",
            "--name",
            "clud-dev",
            "--entrypoint",
            "/bin/bash",
            "-e",
            f"ANTHROPIC_API_KEY={api_key}",
            "-e",
            "CLUD_BACKGROUND_SYNC=true",  # Enable background sync for interactive shell
            "-e",
            "CLUD_SYNC_INTERVAL=300",  # 5 minute sync interval
            "-v",
            f"{docker_path}:/host:rw",
            "-w",
            "/workspace",  # Set working directory to /workspace
        ]

        # Add Claude commands mount if specified
        claude_mount = get_claude_commands_mount(getattr(args, "claude_commands", None))
        if claude_mount:
            host_path, container_path = claude_mount
            base_cmd.extend(["-v", f"{host_path}:{container_path}:ro"])
            print(f"Mounting Claude commands: {host_path} -> {container_path}")

        base_cmd.extend(
            [
                "niteris/clud:latest",
                "--login",  # Login shell to source bashrc and show banner
            ]
        )

        # On Windows with mintty/git-bash, prepend winpty for TTY support
        msystem = os.environ.get("MSYSTEM", "")
        cmd = ["winpty"] + base_cmd if platform.system() == "Windows" and msystem.startswith(("MSYS", "MINGW")) else base_cmd

    print("Starting CLUD development container...")

    try:
        # Set up environment with API key
        env = os.environ.copy()
        env["ANTHROPIC_API_KEY"] = api_key

        if args.cmd:
            # For command execution, use subprocess.run for better control
            result = subprocess.run(cmd, env=env, check=False)
            return result.returncode
        else:
            # For interactive shell, use subprocess.run for direct terminal passthrough
            # This works better on Windows and with various terminal emulators
            result = subprocess.run(cmd, env=env, check=False)
            return result.returncode

    except KeyboardInterrupt:
        print("\nContainer terminated.", file=sys.stderr)
        return 130
    except Exception as e:
        raise DockerError(f"Failed to start container shell: {e}") from e


class BackgroundAgent:
    """Background agent for managing workspace synchronization."""

    def __init__(
        self,
        host_dir: str = "/host",
        workspace_dir: str = "/workspace",
        sync_interval: int = 300,
        watch_mode: bool = False,
    ):
        self.sync_handler = ContainerSync(host_dir, workspace_dir)
        self.host_dir = Path(host_dir)
        self.workspace_dir = Path(workspace_dir)
        self.sync_interval = sync_interval  # seconds
        self.watch_mode = watch_mode
        self.running = False
        self.last_sync_time: datetime | None = None
        self.sync_count = 0
        self.error_count = 0
        self.last_error: str | None = None
        self.state_file = Path("/var/run/clud-bg-agent.state")

        # Set up signal handlers
        signal.signal(signal.SIGTERM, self._handle_signal)
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

    def initial_sync(self) -> bool:
        """Perform initial host → workspace sync."""
        logger.info("Performing initial sync from host to workspace...")
        try:
            exit_code = self.sync_handler.sync_host_to_workspace()
            if exit_code == 0:
                self.last_sync_time = datetime.now()
                self.sync_count += 1
                logger.info("Initial sync completed successfully")
                return True
            else:
                logger.error(f"Initial sync failed with code {exit_code}")
                self.error_count += 1
                self.last_error = f"Initial sync failed with code {exit_code}"
                return False
        except Exception as e:
            logger.error(f"Exception during initial sync: {e}")
            self.error_count += 1
            self.last_error = str(e)
            return False

    def bidirectional_sync(self) -> bool:
        """Perform bidirectional sync between host and workspace."""
        logger.info("Starting bidirectional sync...")
        success = True

        try:
            # First sync workspace changes to host
            logger.debug("Syncing workspace to host...")
            exit_code = self.sync_handler.sync_workspace_to_host()
            if exit_code != 0:
                logger.warning(f"Workspace to host sync failed with code {exit_code}")
                success = False
                self.error_count += 1
                self.last_error = f"Workspace to host sync failed with code {exit_code}"

            # Then sync any host changes back to workspace
            logger.debug("Syncing host to workspace...")
            exit_code = self.sync_handler.sync_host_to_workspace()
            if exit_code != 0:
                logger.warning(f"Host to workspace sync failed with code {exit_code}")
                success = False
                self.error_count += 1
                self.last_error = f"Host to workspace sync failed with code {exit_code}"

            if success:
                self.last_sync_time = datetime.now()
                self.sync_count += 1
                logger.info(f"Bidirectional sync completed (total syncs: {self.sync_count})")
            else:
                logger.warning("Bidirectional sync completed with errors")

            return success

        except Exception as e:
            logger.error(f"Exception during bidirectional sync: {e}")
            self.error_count += 1
            self.last_error = str(e)
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


def main():
    """Main entry point for background agent."""
    parser = argparse.ArgumentParser(description="CLUD background sync agent")
    parser.add_argument("--host-dir", default="/host", help="Host directory path (default: /host)")
    parser.add_argument("--workspace-dir", default="/workspace", help="Workspace directory path (default: /workspace)")
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

    args = parser.parse_args()

    if args.verbose:
        logger.setLevel(logging.DEBUG)
        # Also set container_sync logger to debug
        logging.getLogger("clud.container_sync").setLevel(logging.DEBUG)

    # Validate sync interval
    if args.sync_interval < 10:
        logger.error("Sync interval must be at least 10 seconds")
        sys.exit(1)

    if args.sync_interval > 3600:
        logger.warning("Large sync interval detected (> 1 hour), consider using a smaller interval")

    # Create and run agent
    agent = BackgroundAgent(
        host_dir=args.host_dir,
        workspace_dir=args.workspace_dir,
        sync_interval=args.sync_interval,
        watch_mode=args.watch,
    )

    agent.run()


if __name__ == "__main__":
    main()
