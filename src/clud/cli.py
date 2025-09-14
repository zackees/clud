"""Main CLI entry point for clud."""

import argparse
import contextlib
import os
import platform
import shutil
import subprocess
import sys
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


def get_api_key(args: argparse.Namespace) -> str:
    """Get API key following priority order: --api-key-from, env var, saved config, prompt."""
    api_key = None

    # Priority 1: --api-key-from keyring entry (if keyring is available)
    if args.api_key_from:
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
    image = args.image or "icanhasjonas/claude-docker:latest"
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

    # Check Docker availability
    if not check_docker_available():
        raise DockerError("Docker is not available or not running")

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


def main() -> int:
    """Main entry point for clud."""
    parser = create_parser()
    args = parser.parse_args()

    # Handle conflicting firewall options
    if args.no_firewall:
        args.enable_firewall = False

    try:
        return run_container(args)
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
