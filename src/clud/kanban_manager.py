"""Kanban board manager using vibe-kanban with nodeenv-based Node.js."""

import logging
import subprocess
import sys
from pathlib import Path

logger = logging.getLogger(__name__)


def get_clud_config_dir() -> Path:
    """Get the .clud config directory."""
    return Path.home() / ".clud"


def get_node_env_dir() -> Path:
    """Get the Node.js environment directory."""
    return get_clud_config_dir() / "node22"


def get_plugins_dir() -> Path:
    """Get the plugins directory for npm packages."""
    return get_clud_config_dir() / "plugins"


def is_node_installed() -> bool:
    """Check if Node 22 is already installed in the nodeenv."""
    node_env_dir = get_node_env_dir()
    if sys.platform == "win32":
        node_exe = node_env_dir / "Scripts" / "node.exe"
    else:
        node_exe = node_env_dir / "bin" / "node"
    return node_exe.exists()


def get_node_version() -> str | None:
    """Get the installed Node.js version, if any."""
    if not is_node_installed():
        return None

    node_env_dir = get_node_env_dir()
    if sys.platform == "win32":
        node_exe = node_env_dir / "Scripts" / "node.exe"
    else:
        node_exe = node_env_dir / "bin" / "node"

    try:
        result = subprocess.run(
            [str(node_exe), "--version"],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode == 0:
            return result.stdout.strip()
    except Exception as e:
        logger.warning(f"Failed to get Node.js version: {e}")

    return None


def install_node22() -> bool:
    """Install Node.js 22 LTS using nodeenv."""
    node_env_dir = get_node_env_dir()

    # Check if already installed
    if is_node_installed():
        version = get_node_version()
        print(f"Node.js is already installed at {node_env_dir}")
        if version:
            print(f"Version: {version}")
        return True

    print(f"Installing Node.js 22 LTS to {node_env_dir}...")

    try:
        # Create parent directory if needed
        node_env_dir.parent.mkdir(parents=True, exist_ok=True)

        # Use nodeenv to create Node 22 environment
        # nodeenv --node=22.0.0 path/to/env
        result = subprocess.run(
            [
                sys.executable,
                "-m",
                "nodeenv",
                "--node=22.0.0",
                "--prebuilt",  # Use prebuilt binaries for faster installation
                str(node_env_dir),
            ],
            check=False,
            capture_output=False,  # Show output to user
        )

        if result.returncode != 0:
            print(f"Error: Failed to install Node.js 22 (exit code {result.returncode})", file=sys.stderr)
            return False

        print("✓ Node.js 22 LTS installed successfully")
        version = get_node_version()
        if version:
            print(f"  Version: {version}")
        return True

    except Exception as e:
        print(f"Error: Failed to install Node.js 22: {e}", file=sys.stderr)
        logger.exception("Node.js installation failed")
        return False


def get_npm_path() -> Path | None:
    """Get the path to npm executable."""
    node_env_dir = get_node_env_dir()
    if sys.platform == "win32":
        npm_exe = node_env_dir / "Scripts" / "npm.cmd"
        if not npm_exe.exists():
            npm_exe = node_env_dir / "Scripts" / "npm"
    else:
        npm_exe = node_env_dir / "bin" / "npm"

    return npm_exe if npm_exe.exists() else None


def get_npx_path() -> Path | None:
    """Get the path to npx executable."""
    node_env_dir = get_node_env_dir()
    if sys.platform == "win32":
        npx_exe = node_env_dir / "Scripts" / "npx.cmd"
        if not npx_exe.exists():
            npx_exe = node_env_dir / "Scripts" / "npx"
    else:
        npx_exe = node_env_dir / "bin" / "npx"

    return npx_exe if npx_exe.exists() else None


def is_vibe_kanban_installed() -> bool:
    """Check if vibe-kanban is installed in the plugins directory."""
    plugins_dir = get_plugins_dir()
    vibe_kanban_dir = plugins_dir / "node_modules" / "vibe-kanban"
    return vibe_kanban_dir.exists()


def install_vibe_kanban() -> bool:
    """Install vibe-kanban in the plugins directory."""
    if is_vibe_kanban_installed():
        print("vibe-kanban is already installed")
        return True

    npm_path = get_npm_path()
    if not npm_path:
        print("Error: npm not found. Please ensure Node.js 22 is installed.", file=sys.stderr)
        return False

    plugins_dir = get_plugins_dir()
    plugins_dir.mkdir(parents=True, exist_ok=True)

    print(f"Installing vibe-kanban to {plugins_dir}...")

    try:
        # Install vibe-kanban using npm with --legacy-peer-deps to handle dependency conflicts
        result = subprocess.run(
            [str(npm_path), "install", "vibe-kanban", "--legacy-peer-deps"],
            cwd=str(plugins_dir),
            check=False,
            capture_output=False,  # Show output to user
        )

        if result.returncode != 0:
            print(f"Error: Failed to install vibe-kanban (exit code {result.returncode})", file=sys.stderr)
            return False

        print("✓ vibe-kanban installed successfully")
        return True

    except Exception as e:
        print(f"Error: Failed to install vibe-kanban: {e}", file=sys.stderr)
        logger.exception("vibe-kanban installation failed")
        return False


def run_vibe_kanban() -> int:
    """Run vibe-kanban from the plugins directory."""
    npx_path = get_npx_path()
    if not npx_path:
        print("Error: npx not found. Please ensure Node.js 22 is installed.", file=sys.stderr)
        return 1

    plugins_dir = get_plugins_dir()

    try:
        # Run vibe-kanban using npx
        print("Starting vibe-kanban...")
        result = subprocess.run(
            [str(npx_path), "vibe-kanban"],
            cwd=str(plugins_dir),
            check=False,
        )

        return result.returncode

    except KeyboardInterrupt:
        print("\nvibe-kanban stopped by user")
        return 0
    except Exception as e:
        print(f"Error: Failed to run vibe-kanban: {e}", file=sys.stderr)
        logger.exception("vibe-kanban execution failed")
        return 1


def setup_and_run_kanban() -> int:
    """Ensure Node.js 22 and vibe-kanban are installed, then run vibe-kanban."""
    # Step 1: Install Node.js 22 if needed
    if not install_node22():
        return 1

    # Step 2: Install vibe-kanban if needed
    if not install_vibe_kanban():
        return 1

    # Step 3: Run vibe-kanban
    return run_vibe_kanban()
