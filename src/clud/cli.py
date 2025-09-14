"""Main CLI entry point for clud."""

import argparse
import contextlib
import os
import platform
import shutil
import socket
import subprocess
import sys
import time
import webbrowser
from pathlib import Path

try:
    import keyring
except ImportError:
    keyring = None


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

    parser.add_argument("--just-build", action="store_true", help="Build Docker image and exit (don't launch container)")

    parser.add_argument("--version", action="version", version="clud 0.0.1")

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
        # Convert Windows path to Unix-style for Docker
        path_str = str(path).replace("\\", "/")
        # Handle drive letters: C: -> /c
        if len(path_str) >= 2 and path_str[1] == ":":
            path_str = f"/{path_str[0].lower()}{path_str[2:]}"
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

    # Basic validation: should start with sk-ant- and have reasonable length
    if not api_key.startswith("sk-ant-"):
        return False

    # Should be at least 20 characters (conservative minimum)
    return not len(api_key) < 20


def get_api_key_from_keyring(keyring_name: str) -> str | None:
    """Get API key from OS keyring."""
    if keyring is None:
        raise ConfigError("keyring package not available. Install with: pip install keyring")

    try:
        # Try to get the password from keyring
        api_key = keyring.get_password("clud", keyring_name)
        if not api_key:
            raise ConfigError(f"No API key found in keyring for '{keyring_name}'")
        return api_key
    except Exception as e:
        raise ConfigError(f"Failed to retrieve API key from keyring: {e}") from e


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
        key_file.write_text(api_key, encoding="utf-8")

        # Set restrictive permissions (owner read/write only)
        if platform.system() != "Windows":
            key_file.chmod(0o600)

    except Exception as e:
        raise ConfigError(f"Failed to save API key to config: {e}") from e


def load_api_key_from_config(key_name: str = "anthropic-api-key") -> str | None:
    """Load API key from .clud config directory."""
    try:
        config_dir = get_clud_config_dir()
        key_file = config_dir / f"{key_name}.key"

        if key_file.exists():
            return key_file.read_text(encoding="utf-8").strip()
        return None

    except Exception:
        return None


def prompt_for_api_key() -> str:
    """Interactively prompt user for API key."""
    print("No Claude API key found.")

    while True:
        try:
            api_key = input("Please enter your Anthropic API key: ").strip()
            if not api_key:
                print("API key cannot be empty. Please try again.")
                continue

            if not validate_api_key(api_key):
                print("Invalid API key format. API keys should start with 'sk-ant-' and be at least 20 characters.")
                continue

            # Ask if user wants to save to config
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


def build_docker_image() -> bool:
    """Build the clud-dev Docker image if it doesn't exist."""
    try:
        # Check if image already exists
        result = subprocess.run(["docker", "images", "-q", "clud-dev:latest"], capture_output=True, text=True, check=True)

        if result.stdout.strip():
            print("Docker image clud-dev:latest already exists")
            return True

        print("Building clud-dev Docker image...")

        # Build the image from the current directory's Dockerfile
        result = subprocess.run(["docker", "build", "-t", "clud-dev:latest", "."], check=True)

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


def run_ui_container(args: argparse.Namespace, project_path: Path, api_key: str) -> int:
    """Run the code-server UI container."""
    # Find available port
    port = args.port
    if not is_port_available(port):
        print(f"Port {port} is not available, finding alternative...")
        port = find_available_port(port)
        print(f"Using port {port}")

    # Build image if not already built
    if (not hasattr(args, "_image_built") or not args._image_built) and not build_docker_image():
        return 1

    # Stop existing container
    stop_existing_container()

    # Prepare Docker command
    docker_path = normalize_path_for_docker(project_path)
    # Note: Not mounting .local to preserve container's installed tools (Claude CLI, etc.)
    # Only mount .config for user settings
    home_config_path = normalize_path_for_docker(Path.home() / ".config")

    cmd = [
        "docker",
        "run",
        "-d",
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
        "clud-dev:latest",
    ]

    print(f"Starting clud-dev container on port {port}...")

    try:
        result = subprocess.run(cmd, check=True, capture_output=True, text=True)
        container_id = result.stdout.strip()
        print(f"Container started with ID: {container_id[:12]}")

        # Wait a moment for the server to start
        print("Waiting for code-server to start...")
        time.sleep(3)

        # Check if container is still running
        check_result = subprocess.run(["docker", "ps", "-q", "-f", f"id={container_id}"], capture_output=True, text=True, check=True)

        if not check_result.stdout.strip():
            # Container stopped, get logs
            logs_result = subprocess.run(["docker", "logs", container_id], capture_output=True, text=True)
            print(f"Container failed to start. Logs:\n{logs_result.stdout}\n{logs_result.stderr}")
            return 1

        # Open browser
        url = f"http://localhost:{port}"
        print(f"Opening browser to {url}")
        try:
            webbrowser.open(url)
        except Exception as e:
            print(f"Could not open browser automatically: {e}")
            print(f"Please open {url} in your browser")

        print(f"""
Code-server is now running!
- URL: {url}
- Container: clud-dev
- Project: {project_path}

To stop the container: docker stop clud-dev
To view logs: docker logs clud-dev
""")

        return 0

    except subprocess.CalledProcessError as e:
        print(f"Failed to start container: {e}")
        if e.stderr:
            print(f"Error: {e.stderr}")
        return 1


def get_api_key(args: argparse.Namespace) -> str:
    """Get API key following priority order: --api-key, --api-key-from, env var, saved config, prompt."""
    api_key = None

    # Priority 0: --api-key command line argument
    if hasattr(args, "api_key") and args.api_key:
        api_key = args.api_key

    # Priority 1: --api-key-from keyring entry (if keyring is available)
    if not api_key and args.api_key_from:
        with contextlib.suppress(ConfigError):
            api_key = get_api_key_from_keyring(args.api_key_from) if keyring is not None else load_api_key_from_config(args.api_key_from)

    # Priority 2: Environment variable
    if not api_key:
        api_key = os.environ.get("ANTHROPIC_API_KEY")

    # Priority 3: Saved config file
    if not api_key:
        api_key = load_api_key_from_config()

    # Priority 4: Interactive prompt
    if not api_key:
        api_key = prompt_for_api_key()

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

    cmd = ["docker", "run", "-it", "--rm", f"--name=clud-{project_name}", f"--volume={docker_path}:/workspace:rw"]

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
    image = args.image or "icanhasjonas/claude-code:latest"
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
    """Launch container and drop user into bash shell at /workspace."""
    # Validate project path
    project_path = validate_path(args.path)

    # Get API key
    api_key = get_api_key(args)

    # Docker availability already checked in main()

    # Build image if not already built
    if (not hasattr(args, "_image_built") or not args._image_built) and not build_docker_image():
        return 1

    # Stop existing container
    stop_existing_container()

    # Prepare Docker command for interactive shell
    docker_path = normalize_path_for_docker(project_path)

    cmd = [
        "docker",
        "run",
        "-it",
        "--rm",
        "--name",
        "clud-dev",
        "-e",
        f"ANTHROPIC_API_KEY={api_key}",
        "-v",
        f"{docker_path}:/workspace",
        "-w",
        "/workspace",  # Set working directory to /workspace
        "clud-dev:latest",
        "/bin/bash",
        "-c",
        # Custom startup script that shows banner and starts bash
        """
        # Function to show banner
        show_banner() {
            clud --show-banner 2>/dev/null || true
        }

        # Show banner on first run
        show_banner

        # Wait for user to press enter
        echo -n ""
        read -p "" dummy 2>/dev/null || true

        # Clear screen and show prompt
        clear
        echo "┌─ CLUD Development Environment ─────────────────────────────────────┐"
        echo "│ Working Directory: /workspace                                      │"
        echo "│ Type 'clud' to start the background agent                         │"
        echo "└────────────────────────────────────────────────────────────────────┘"
        echo ""

        # Start interactive bash
        exec /bin/bash
        """,
    ]

    print("Starting CLUD development container...")

    try:
        # Set up environment with API key
        env = os.environ.copy()
        env["ANTHROPIC_API_KEY"] = api_key

        # Execute the container
        result = subprocess.run(cmd, env=env, check=False)
        return result.returncode

    except Exception as e:
        raise DockerError(f"Failed to start container shell: {e}") from e


def main() -> int:
    """Main entry point for clud."""
    # clud CLI is only used outside containers to launch development environments
    # Inside containers, 'clud' is a bash alias to 'claude code --dangerously-skip-permissions'

    parser = create_parser()
    args = parser.parse_args()

    # Handle conflicting firewall options
    if args.no_firewall:
        args.enable_firewall = False

    try:
        # Check Docker availability first for all modes that need Docker
        if not check_docker_available():
            raise DockerError("Docker is not available or not running")

        # Handle build-only mode
        if args.just_build:
            print("Building Docker image...")
            if build_docker_image():
                print("Docker image built successfully!")
                return 0
            else:
                print("Failed to build Docker image", file=sys.stderr)
                return 1

        # Force build if requested
        if args.build:
            print("Building Docker image...")
            if not build_docker_image():
                print("Failed to build Docker image", file=sys.stderr)
                return 1
            args._image_built = True

        # Route to different modes
        if args.ui:
            # UI mode - launch code-server container
            project_path = validate_path(args.path)
            api_key = get_api_key(args)

            return run_ui_container(args, project_path, api_key)
        else:
            # Default mode - launch container with interactive shell
            return launch_container_shell(args)

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
