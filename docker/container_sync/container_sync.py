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
    # Change to a stable working directory to avoid getcwd issues
    original_cwd = os.getcwd()
    os.chdir("/")

    try:
        cmd = ["rsync", "-av", "--delete"]

        # Add exclusions
        for pattern in EXCLUDE_PATTERNS:
            cmd.extend(["--exclude", pattern])

        # Exclude .git when syncing back to host
        if exclude_git:
            cmd.extend(["--exclude", ".git"])

        cmd.extend([f"{source}/", f"{dest}/"])

        result = subprocess.run(cmd, check=True)
        print(f"[DOCKER] Sync completed: {source} -> {dest}")
        return True
    except subprocess.CalledProcessError as e:
        print(f"[DOCKER] Sync failed: {e}")
        return False
    finally:
        # Restore original working directory
        try:
            os.chdir(original_cwd)
        except OSError:
            # If original directory is gone, stay in root
            pass


def setup_git_workspace():
    """Set up Git workspace using worktree for optimal performance."""
    host_path = Path(HOST_DIR)
    workspace_path = Path(WORKSPACE_DIR)
    host_git_dir = host_path / ".git"

    # Check if host has a .git directory
    if not host_git_dir.exists():
        print("[DOCKER] No .git directory found in host, skipping Git setup")
        return False

    try:
        # Clean workspace if it already exists and is a worktree
        if workspace_path.exists():
            git_file = workspace_path / ".git"
            if git_file.exists() and git_file.is_file():
                # This is a worktree, remove it properly
                try:
                    subprocess.run([
                        "git", f"--git-dir={host_git_dir}",
                        "worktree", "remove", "--force", str(workspace_path)
                    ], check=True, capture_output=True)
                except subprocess.CalledProcessError:
                    # If worktree remove fails, just delete the directory
                    subprocess.run(["rm", "-rf", str(workspace_path)], check=True)
            else:
                # Regular directory, just remove it
                subprocess.run(["rm", "-rf", str(workspace_path)], check=True)

        # Create parent directory
        workspace_path.parent.mkdir(parents=True, exist_ok=True)

        # Change to a stable working directory to avoid rsync getcwd issues
        os.chdir("/")

        # Get the current branch name from the host repo
        branch_cmd = ["git", f"--git-dir={host_git_dir}", "rev-parse", "--abbrev-ref", "HEAD"]
        branch_result = subprocess.run(branch_cmd, capture_output=True, text=True, check=True)
        current_branch = branch_result.stdout.strip()

        # Create worktree pointing to the current commit (detached HEAD) to avoid branch conflicts
        commit_cmd = ["git", f"--git-dir={host_git_dir}", "rev-parse", "HEAD"]
        commit_result = subprocess.run(commit_cmd, capture_output=True, text=True, check=True)
        current_commit = commit_result.stdout.strip()

        # Create worktree pointing to the current commit (detached)
        cmd = [
            "git",
            f"--git-dir={host_git_dir}",
            "worktree", "add", "--detach",
            str(workspace_path),
            current_commit
        ]

        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        print(f"[DOCKER] Git worktree created successfully for branch: {current_branch}")
        return True

    except subprocess.CalledProcessError as e:
        print(f"[DOCKER] Failed to create Git worktree: {e}")
        print(f"[DOCKER] stdout: {e.stdout}")
        print(f"[DOCKER] stderr: {e.stderr}")
        # Ensure workspace directory exists for fallback
        workspace_path.mkdir(parents=True, exist_ok=True)
        return False


def init_container():
    """Initialize container by syncing host to workspace."""
    host_path = Path(HOST_DIR)
    workspace_path = Path(WORKSPACE_DIR)

    if not host_path.exists():
        print("[DOCKER] No host directory found, skipping sync")
        return 0

    # Try to set up Git workspace first
    git_setup_success = setup_git_workspace()

    if git_setup_success:
        # Git worktree setup successful, now sync non-git files
        print("[DOCKER] Syncing non-Git files...")
        # Sync everything except .git (it's handled by worktree)
        if sync_with_rsync(HOST_DIR, WORKSPACE_DIR, exclude_git=True):
            print("[DOCKER] Initial sync completed with Git worktree")
        else:
            print("[DOCKER] Warning: File sync failed, but Git worktree is set up")
    else:
        # Fallback: sync all files including .git if worktree setup failed
        print("[DOCKER] Falling back to full file sync including .git...")
        if sync_with_rsync(HOST_DIR, WORKSPACE_DIR, exclude_git=False):
            print("[DOCKER] Initial sync completed with .git fallback")
        else:
            return 1

    # Configure Git safe directories to handle ownership issues
    if workspace_path.exists() and (workspace_path / ".git").exists():
        try:
            # Configure Git to treat workspace as safe directory
            subprocess.run([
                "git", "config", "--global", "--add", "safe.directory", WORKSPACE_DIR
            ], capture_output=True, check=False)
            print("[DOCKER] Git safe directory configured")
        except Exception as e:
            print(f"[DOCKER] Warning: Failed to configure Git safe directory: {e}")

    # Configure code-server
    config_dir = Path("/home/coder/.config/code-server")
    config_dir.mkdir(parents=True, exist_ok=True)

    config_file = config_dir / "config.yaml"
    config_file.write_text("bind-addr: 0.0.0.0:8080\nauth: none\ncert: false\n")

    # Fix permissions
    subprocess.run(["chown", "-R", "coder:coder", "/home/coder/.config"], check=False)
    subprocess.run(["chown", "-R", "coder:coder", WORKSPACE_DIR], check=False)

    return 0


def cleanup_git_workspace():
    """Clean up Git worktree if it exists."""
    host_path = Path(HOST_DIR)
    workspace_path = Path(WORKSPACE_DIR)
    host_git_dir = host_path / ".git"

    if not host_git_dir.exists() or not workspace_path.exists():
        return True

    try:
        # Check if workspace is a Git worktree
        git_file = workspace_path / ".git"
        if git_file.exists() and git_file.is_file():
            # This is likely a worktree - clean it up properly
            cmd = [
                "git",
                f"--git-dir={host_git_dir}",
                "worktree", "remove", "--force",
                str(workspace_path)
            ]
            subprocess.run(cmd, capture_output=True, text=True, check=True)
            print("[DOCKER] Git worktree cleaned up")

        return True

    except subprocess.CalledProcessError as e:
        print(f"[DOCKER] Warning: Failed to clean up Git worktree: {e}")
        return False


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
    parser.add_argument("command", choices=["init", "sync", "cleanup"], help="Command to execute")

    args = parser.parse_args()

    if args.command == "init":
        return init_container()
    elif args.command == "sync":
        return sync_back()
    elif args.command == "cleanup":
        cleanup_git_workspace()
        return 0

    return 0


if __name__ == "__main__":
    sys.exit(main())
