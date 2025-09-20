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
    """DISABLED: Sync directories using rsync - disabled per FEATURE.md directive."""
    print(f"[DOCKER] RSYNC DISABLED: Skipping sync {source} -> {dest}")
    print(f"[DOCKER] Using Git worktree for workspace setup instead")
    return True


def setup_git_workspace():
    """Set up Git workspace using worktree for optimal performance."""
    host_path = Path(HOST_DIR)
    workspace_path = Path(WORKSPACE_DIR)
    host_git_dir = host_path / ".git"

    print(f"[DOCKER] Setting up Git workspace...")
    print(f"[DOCKER] Host path: {host_path}")
    print(f"[DOCKER] Workspace path: {workspace_path}")
    print(f"[DOCKER] Host git dir: {host_git_dir}")

    # Check if host has a .git directory
    if not host_git_dir.exists():
        print("[DOCKER] No .git directory found in host, skipping Git setup")
        return False

    print(f"[DOCKER] Git directory found, proceeding with setup...")

    try:
        # Prune stale worktree entries first
        try:
            subprocess.run([
                "git", f"--git-dir={host_git_dir}",
                "worktree", "prune"
            ], capture_output=True, check=True, timeout=60)
        except subprocess.CalledProcessError:
            # Ignore pruning errors
            pass

        # Clean workspace if it already exists and is a worktree
        if workspace_path.exists():
            print(f"[DOCKER] Workspace {workspace_path} exists, cleaning up...")
            git_file = workspace_path / ".git"
            if git_file.exists() and git_file.is_file():
                # This is a worktree, remove it properly
                print(f"[DOCKER] Removing existing worktree...")
                try:
                    subprocess.run([
                        "git", f"--git-dir={host_git_dir}",
                        "worktree", "remove", "--force", str(workspace_path)
                    ], check=True, capture_output=True, timeout=60)
                    print(f"[DOCKER] Worktree removed successfully")
                except subprocess.CalledProcessError as e:
                    print(f"[DOCKER] Worktree remove failed: {e}")
                    # If worktree remove fails, clean up contents instead of removing directory
                    # (directory might be a mount point and cannot be removed)
                    subprocess.run(["find", str(workspace_path), "-mindepth", "1", "-delete"], check=False)
            else:
                print(f"[DOCKER] Regular directory, cleaning contents...")
                # Regular directory, clean contents instead of removing directory
                # (directory might be a mount point and cannot be removed)
                subprocess.run(["find", str(workspace_path), "-mindepth", "1", "-delete"], check=False)

        # Create parent directory
        workspace_path.parent.mkdir(parents=True, exist_ok=True)

        # Change to a stable working directory to avoid rsync getcwd issues
        os.chdir("/")

        # Configure Git safe directory before any worktree operations
        try:
            subprocess.run([
                "git", "config", "--global", "--add", "safe.directory", WORKSPACE_DIR
            ], capture_output=True, check=False, timeout=30)
            print("[DOCKER] Git safe directory configured before worktree creation")
        except Exception as e:
            print(f"[DOCKER] Warning: Failed to configure Git safe directory: {e}")

        # Get the current branch name from the host repo
        branch_cmd = ["git", f"--git-dir={host_git_dir}", "rev-parse", "--abbrev-ref", "HEAD"]
        branch_result = subprocess.run(branch_cmd, capture_output=True, text=True, check=True, timeout=30, encoding='utf-8', errors='replace')
        current_branch = branch_result.stdout.strip()
        workspace_branch = f"workspace-{current_branch}"

        # Check if workspace branch exists
        branch_check_cmd = ["git", f"--git-dir={host_git_dir}", "rev-parse", "--verify", f"refs/heads/{workspace_branch}"]
        branch_exists = subprocess.run(branch_check_cmd, capture_output=True, check=False, timeout=30).returncode == 0

        if branch_exists:
            # Try to delete the existing branch
            try:
                subprocess.run([
                    "git", f"--git-dir={host_git_dir}",
                    "branch", "-D", workspace_branch
                ], capture_output=True, check=True, timeout=30)
                print(f"[DOCKER] Cleaned up existing workspace branch: {workspace_branch}")
                branch_exists = False  # Mark as deleted
            except subprocess.CalledProcessError:
                print(f"[DOCKER] Could not delete existing branch {workspace_branch}, will reuse it")

        # Create worktree - use -b only if branch doesn't exist
        if branch_exists:
            # Branch exists but couldn't be deleted, use it without -b
            cmd = [
                "git",
                f"--git-dir={host_git_dir}",
                "worktree", "add",
                str(workspace_path),
                workspace_branch
            ]
        else:
            # Branch doesn't exist or was deleted, create new one
            cmd = [
                "git",
                f"--git-dir={host_git_dir}",
                "worktree", "add",
                "-b", workspace_branch,
                str(workspace_path),
                current_branch
            ]

        print(f"[DOCKER] Running command: {' '.join(cmd)}")
        result = subprocess.run(cmd, capture_output=True, text=True, check=True, timeout=300, encoding='utf-8', errors='replace')
        print(f"[DOCKER] Git worktree created successfully for branch: {current_branch}")
        if result.stdout:
            print(f"[DOCKER] stdout: {result.stdout}")
        if result.stderr:
            print(f"[DOCKER] stderr: {result.stderr}")

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
        print("[DOCKER] Git worktree setup successful - RSYNC DISABLED per FEATURE.md directive")
        print("[DOCKER] Workspace is ready using Git worktree only")
    else:
        print("[DOCKER] Git worktree setup failed - RSYNC DISABLED per FEATURE.md directive")
        print("[DOCKER] Manual setup may be required for non-Git projects")
        # Create workspace directory for consistency
        workspace_path.mkdir(parents=True, exist_ok=True)

    # Configure Git safe directories to handle ownership issues
    if workspace_path.exists() and (workspace_path / ".git").exists():
        try:
            # Configure Git to treat workspace as safe directory for root user
            subprocess.run([
                "git", "config", "--global", "--add", "safe.directory", WORKSPACE_DIR
            ], capture_output=True, check=False, timeout=30)
            print("[DOCKER] Git safe directory configured for root user")
        except Exception as e:
            print(f"[DOCKER] Warning: Failed to configure Git safe directory: {e}")

    # Configure code-server for current user
    import os
    current_user = os.getenv("USER", "root")
    home_dir = os.path.expanduser("~")
    config_dir = Path(f"{home_dir}/.config/code-server")
    config_dir.mkdir(parents=True, exist_ok=True)

    config_file = config_dir / "config.yaml"
    config_file.write_text("bind-addr: 0.0.0.0:8080\nauth: none\ncert: false\n")

    # Ensure workspace ownership (only if running as root)
    if current_user == "root":
        subprocess.run(["chown", "-R", "root:root", WORKSPACE_DIR], check=False)
    else:
        # Non-root users typically don't need to change ownership
        # The mounted files should already be accessible
        print(f"[DOCKER] Running as non-root user '{current_user}' - skipping workspace ownership change")

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
            subprocess.run(cmd, capture_output=True, text=True, check=True, timeout=60, encoding='utf-8', errors='replace')
            print("[DOCKER] Git worktree cleaned up")

        return True

    except subprocess.CalledProcessError as e:
        print(f"[DOCKER] Warning: Failed to clean up Git worktree: {e}")
        return False


def sync_back():
    """DISABLED: Sync workspace back to host - disabled per FEATURE.md directive."""
    workspace_path = Path(WORKSPACE_DIR)

    if not workspace_path.exists():
        print("[DOCKER] No workspace directory found")
        return 1

    print("[DOCKER] RSYNC DISABLED: Skipping sync back to host")
    print("[DOCKER] Using Git worktree - changes are automatically reflected in host")
    return 0


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
