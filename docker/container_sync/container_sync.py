#!/usr/bin/env python3
"""Simple container sync script for CLUD development environment."""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

# Simple configuration
HOST_DIR = "/host"
WORKSPACE_DIR = "/workspace"

# Files to exclude from sync
EXCLUDE_PATTERNS = [
    ".DS_Store",
    "__pycache__",
    "*.pyc",
    ".pytest_cache",
    "node_modules",
    "dist",
    "build",
    ".venv",
    ".env",
    ".docker_test_cache.json",
]


def sync_with_rsync(source, dest, exclude_git=False):
    """Sync directories using rsync."""
    cmd = ["rsync", "-av", "--delete"]

    # Add exclusions
    for pattern in EXCLUDE_PATTERNS:
        cmd.extend(["--exclude", pattern])

    # Exclude .git when syncing back to host
    if exclude_git:
        cmd.extend(["--exclude", ".git"])

    cmd.extend([f"{source}/", f"{dest}/"])

    try:
        result = subprocess.run(cmd, check=True)
        print(f"[DOCKER] Sync completed: {source} -> {dest}")
        return True
    except subprocess.CalledProcessError as e:
        print(f"[DOCKER] Sync failed: {e}")
        return False


def init_container():
    """Initialize container by syncing host to workspace."""
    host_path = Path(HOST_DIR)
    workspace_path = Path(WORKSPACE_DIR)

    if not host_path.exists():
        print("[DOCKER] No host directory found, skipping sync")
        return 0

    # Ensure workspace directory exists
    workspace_path.mkdir(parents=True, exist_ok=True)

    # Sync host to workspace (allows .git)
    if sync_with_rsync(HOST_DIR, WORKSPACE_DIR, exclude_git=False):
        print("[DOCKER] Initial sync completed")

        # Configure code-server
        config_dir = Path("/home/coder/.config/code-server")
        config_dir.mkdir(parents=True, exist_ok=True)

        config_file = config_dir / "config.yaml"
        config_file.write_text("bind-addr: 0.0.0.0:8080\nauth: none\ncert: false\n")

        # Fix permissions
        subprocess.run(["chown", "-R", "coder:coder", "/home/coder/.config"], check=False)
        subprocess.run(["chown", "-R", "coder:coder", WORKSPACE_DIR], check=False)

        return 0
    else:
        return 1


def sync_back():
    """Sync workspace back to host (excludes .git)."""
    workspace_path = Path(WORKSPACE_DIR)

    if not workspace_path.exists():
        print("[DOCKER] No workspace directory found")
        return 1

    # Sync workspace to host (excludes .git)
    if sync_with_rsync(WORKSPACE_DIR, HOST_DIR, exclude_git=True):
        print("[DOCKER] Sync back completed")
        return 0
    else:
        return 1


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(description="Simple container sync utility")
    parser.add_argument("command", choices=["init", "sync"], help="Command to execute")

    args = parser.parse_args()

    if args.command == "init":
        return init_container()
    elif args.command == "sync":
        return sync_back()

    return 0


if __name__ == "__main__":
    sys.exit(main())
