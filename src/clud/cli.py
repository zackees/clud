"""Main CLI entry point for clud."""

import argparse
import contextlib
import json
import os
import platform
import shutil
import socket
import subprocess
import sys
import threading
import time
import webbrowser
from pathlib import Path
from typing import Any

from .secrets import get_credential_store

# Get credential store once at module level
keyring = get_credential_store()


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

    parser.add_argument("--yolo", action="store_true", help="Launch Claude Code with dangerous permissions (bypasses all safety prompts)")

    return parser


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
        "-v",
        f"{docker_path}:/home/coder/project",
        "-v",
        f"{home_config_path}:/home/coder/.config",
        # Removed .local mount to preserve container's installed CLI tools
        "niteris/clud:latest",
    ]

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


def get_api_key(args: argparse.Namespace) -> str:
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


def run_container(args: argparse.Namespace) -> int:
    """Main logic to run the container."""
    # Validate project path
    project_path = validate_path(args.path)

    # Get API key - this is required before running Claude
    api_key = get_api_key(args)

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


def launch_container_shell(args: argparse.Namespace) -> int:
    """Launch container and drop user into bash shell at /workspace or execute specified command."""
    # Validate project path
    project_path = validate_path(args.path)

    # Get API key
    api_key = get_api_key(args)

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
        cmd = [
            "docker",
            "run",
            "--rm",
            "--name",
            "clud-dev",
            "-e",
            f"ANTHROPIC_API_KEY={api_key}",
            "-v",
            f"{docker_path}:/host",
            "-w",
            "/workspace",  # Set working directory to /workspace
            "--entrypoint",
            "/bin/bash",
            "niteris/clud:latest",
            "-c",
            args.cmd,
        ]
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
            "-v",
            f"{docker_path}:/host",
            "-w",
            "/workspace",  # Set working directory to /workspace
            "niteris/clud:latest",
            "--login",  # Login shell to source bashrc and show banner
        ]

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


def main() -> int:
    """Main entry point for clud."""
    # Simple arg parser for key flags
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--bg", "-bg", action="store_true", help="Use background mode")

    # Parse known args, keep unknown args
    known_args, unknown_args = parser.parse_known_args()

    if known_args.bg:
        from .bg import main as bg_main

        return bg_main(unknown_args)
    else:
        from .yolo import main as yolo_main

        return yolo_main(unknown_args)


if __name__ == "__main__":
    sys.exit(main())
