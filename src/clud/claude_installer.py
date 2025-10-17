"""Claude Code installation manager for clud.

This module handles automatic installation and management of Claude Code
in a local, self-contained directory to avoid PATH and npm global issues.
"""

import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

from running_process import RunningProcess


def get_clud_bin_dir() -> Path:
    """Get or create the ~/.clud/bin directory for Claude Code binaries."""
    bin_dir = Path.home() / ".clud" / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    return bin_dir


def get_clud_npm_dir() -> Path:
    """Get or create the ~/.clud/npm directory for npm packages."""
    npm_dir = Path.home() / ".clud" / "npm"
    npm_dir.mkdir(parents=True, exist_ok=True)
    return npm_dir


def get_local_claude_path() -> Path | None:
    """Get path to locally-installed Claude Code in ~/.clud/npm."""
    npm_dir = get_clud_npm_dir()

    # Check for .cmd file on Windows (npm global install creates .cmd wrapper)
    if platform.system() == "Windows":
        claude_cmd = npm_dir / "claude.cmd"
        if claude_cmd.exists():
            return claude_cmd

        # Also check for .exe (less common but possible)
        claude_exe = npm_dir / "claude.exe"
        if claude_exe.exists():
            return claude_exe

    # On Unix or as fallback, check for bin/claude
    claude_bin = npm_dir / "bin" / "claude"
    if claude_bin.exists():
        return claude_bin

    # Check node_modules/.bin/claude (where npm link creates scripts)
    claude_node_bin = npm_dir / "node_modules" / ".bin" / "claude"
    if claude_node_bin.exists():
        return claude_node_bin

    # Windows: Also check node_modules/.bin/claude.cmd
    if platform.system() == "Windows":
        claude_node_cmd = npm_dir / "node_modules" / ".bin" / "claude.cmd"
        if claude_node_cmd.exists():
            return claude_node_cmd

    return None


def find_npm_executable() -> str | None:
    """Find npm executable, preferring bundled nodejs-wheel version."""
    # Try to use npm from nodejs-wheel (bundled with clud)
    try:
        # Check if we're in a virtual environment with nodejs-wheel
        import nodejs_wheel

        # Get the nodejs_wheel package directory
        nodejs_dir = Path(nodejs_wheel.__file__).parent

        # Look for npm in the nodejs-wheel bin directory
        if platform.system() == "Windows":
            npm_paths = [
                nodejs_dir / "bin" / "npm.cmd",
                nodejs_dir / "bin" / "npm.exe",
                nodejs_dir / "Scripts" / "npm.cmd",
                nodejs_dir / "Scripts" / "npm.exe",
            ]
        else:
            npm_paths = [
                nodejs_dir / "bin" / "npm",
            ]

        for npm_path in npm_paths:
            if npm_path.exists():
                return str(npm_path)
    except (ImportError, AttributeError):
        # nodejs_wheel not available or doesn't have expected structure
        pass

    # Fall back to system npm
    npm_path = shutil.which("npm")
    if npm_path:
        return npm_path

    # On Windows, try common npm locations
    if platform.system() == "Windows":
        possible_locations = [
            Path(os.environ.get("APPDATA", "")) / "npm" / "npm.cmd",
            Path("C:/Program Files/nodejs/npm.cmd"),
            Path("C:/Program Files/nodejs/npm.exe"),
        ]
        for location in possible_locations:
            if location.exists():
                return str(location)

    return None


def is_claude_installed_locally() -> bool:
    """Check if Claude Code is installed in ~/.clud/npm."""
    return get_local_claude_path() is not None


def install_claude_local(verbose: bool = False) -> bool:
    """Install Claude Code to ~/.clud/npm directory.

    Args:
        verbose: Whether to show detailed output

    Returns:
        True if installation succeeded, False otherwise
    """
    npm_dir = get_clud_npm_dir()

    # Find npm executable
    npm_path = find_npm_executable()
    if not npm_path:
        print("Error: npm not found. Please install Node.js.", file=sys.stderr)
        return False

    if verbose:
        print(f"Using npm: {npm_path}", file=sys.stderr)
        print(f"Installing to: {npm_dir}", file=sys.stderr)

    # Prepare npm install command
    # Use --prefix to install to specific directory
    cmd = [
        npm_path,
        "install",
        "--prefix",
        str(npm_dir),
        "--global-style",  # Install in flat structure (avoids nested node_modules)
        "@anthropic-ai/claude-code@latest",
    ]

    if verbose:
        print(f"Running: {' '.join(cmd)}", file=sys.stderr)

    try:
        # Run npm install with streaming output
        returncode = RunningProcess.run_streaming(cmd)

        if returncode != 0:
            print(f"Error: npm install exited with code {returncode}", file=sys.stderr)
            return False

        # Verify installation
        claude_path = get_local_claude_path()
        if claude_path and claude_path.exists():
            print(f"\n✓ Claude Code installed successfully to {claude_path}", file=sys.stderr)
            return True
        else:
            print("\nError: Installation succeeded but claude executable not found", file=sys.stderr)
            print(f"Expected location: {npm_dir}", file=sys.stderr)
            return False

    except Exception as e:
        print(f"Error during installation: {e}", file=sys.stderr)
        return False


def prompt_install_claude() -> bool:
    """Interactively prompt user to install Claude Code locally.

    Returns:
        True if user chose to install and installation succeeded, False otherwise
    """
    print("\nClaude Code is not installed.", file=sys.stderr)
    print("Would you like to install it to ~/.clud/npm? (Recommended)", file=sys.stderr)
    print("This will use npm to install @anthropic-ai/claude-code locally.", file=sys.stderr)
    print(file=sys.stderr)

    try:
        sys.stdout.flush()
        response = input("Install Claude Code? [Y/n]: ").strip().lower()

        if response in ["", "y", "yes"]:
            print(file=sys.stderr)
            print("Installing Claude Code...", file=sys.stderr)
            return install_claude_local(verbose=True)
        else:
            print("\nInstallation cancelled.", file=sys.stderr)
            print("You can install manually with: clud --install-claude", file=sys.stderr)
            return False

    except (EOFError, KeyboardInterrupt):
        print("\n\nInstallation cancelled.", file=sys.stderr)
        return False


def get_claude_version(claude_path: str) -> str | None:
    """Get version of Claude Code executable.

    Args:
        claude_path: Path to claude executable

    Returns:
        Version string or None if unable to determine
    """
    try:
        result = subprocess.run(
            [claude_path, "--version"],
            capture_output=True,
            text=True,
            timeout=5.0,
            check=False,
        )

        if result.returncode == 0:
            # Version output is typically on stdout
            version = result.stdout.strip()
            if version:
                return version

        return None
    except Exception:
        return None


def find_claude_code() -> str | None:
    """Find Claude Code executable, with intelligent fallback logic.

    Priority order:
    1. Locally installed in ~/.clud/npm (recommended)
    2. System PATH (global npm install)
    3. Common Windows locations

    Returns:
        Path to claude executable or None if not found
    """
    # Priority 1: Check local installation
    local_path = get_local_claude_path()
    if local_path:
        return str(local_path)

    # Priority 2: Check system PATH
    if platform.system() == "Windows":
        # On Windows, prefer .cmd and .exe extensions
        claude_path = shutil.which("claude.cmd") or shutil.which("claude.exe")
        if claude_path:
            return claude_path

    # Generic "claude" (for Unix or git bash on Windows)
    claude_path = shutil.which("claude")
    if claude_path:
        return claude_path

    # Priority 3: Check common Windows npm global locations
    if platform.system() == "Windows":
        possible_paths = [
            Path(os.environ.get("APPDATA", "")) / "npm" / "claude.cmd",
            Path(os.environ.get("APPDATA", "")) / "npm" / "claude.exe",
            Path("C:/Users") / os.environ.get("USERNAME", "") / "AppData" / "Roaming" / "npm" / "claude.cmd",
        ]
        for path in possible_paths:
            if path.exists():
                return str(path)

    return None


def uninstall_claude_local(verbose: bool = False) -> bool:
    """Uninstall Claude Code from ~/.clud/npm directory.

    Args:
        verbose: Whether to show detailed output

    Returns:
        True if uninstallation succeeded, False otherwise
    """
    npm_dir = get_clud_npm_dir()

    if not npm_dir.exists():
        if verbose:
            print("No local installation found.", file=sys.stderr)
        return True

    # Find npm executable
    npm_path = find_npm_executable()
    if not npm_path:
        # If npm is not available, just remove the directory
        if verbose:
            print("npm not found, removing directory directly...", file=sys.stderr)
        try:
            shutil.rmtree(npm_dir)
            print("✓ Local Claude Code installation removed", file=sys.stderr)
            return True
        except Exception as e:
            print(f"Error removing directory: {e}", file=sys.stderr)
            return False

    # Use npm uninstall
    cmd = [
        npm_path,
        "uninstall",
        "--prefix",
        str(npm_dir),
        "@anthropic-ai/claude-code",
    ]

    if verbose:
        print(f"Running: {' '.join(cmd)}", file=sys.stderr)

    try:
        returncode = RunningProcess.run_streaming(cmd)

        if returncode == 0:
            print("✓ Claude Code uninstalled successfully", file=sys.stderr)
            return True
        else:
            print(f"Warning: npm uninstall exited with code {returncode}", file=sys.stderr)
            return False

    except Exception as e:
        print(f"Error during uninstallation: {e}", file=sys.stderr)
        return False
