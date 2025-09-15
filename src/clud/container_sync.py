#!/usr/bin/env python3
"""Container sync functionality for CLUD development environment."""

import argparse
import contextlib
import logging
import os
import subprocess
import sys
import time
from pathlib import Path

# ==================== CONSTANTS ====================
# Exit codes
READONLY_ERROR = 10
SYNC_ERROR = 11
PERMISSION_ERROR = 12

# Default paths
DEFAULT_HOST_DIR = "/host"
DEFAULT_WORKSPACE_DIR = "/workspace"
DEFAULT_LOG_FILE = "/var/log/clud-sync.log"

# Sync configuration
MAX_RETRIES = 3
RETRY_DELAY_SECONDS = 2

# Rsync exclusions (shared by both directions)
RSYNC_EXCLUSIONS_COMMON = [
    "/.docker_test_cache.json",
    "**/.DS_Store",
    "**/__pycache__",
    "**/*.pyc",
    "**/.pytest_cache",
    "**/node_modules",
    "**/dist",
    "**/build",
    "**/.venv",
    "**/.env",
]

# Additional exclusions for container->host sync (includes .git)
RSYNC_EXCLUSIONS_TO_HOST = RSYNC_EXCLUSIONS_COMMON + [
    "/.git",  # .git must NOT be synced back to host
]

# Code-server configuration
CODE_SERVER_BIND_ADDR = "0.0.0.0:8080"
CODE_SERVER_CONFIG = """bind-addr: 0.0.0.0:8080
auth: none
cert: false
"""

# ==================== LOGGING SETUP ====================
logging.basicConfig(level=logging.INFO, format="[%(asctime)s] [%(levelname)s] [container-sync] %(message)s", datefmt="%Y-%m-%d %H:%M:%S")
logger = logging.getLogger(__name__)


class ContainerSync:
    """Handles bidirectional sync between host and workspace directories."""

    def __init__(self, host_dir: str = DEFAULT_HOST_DIR, workspace_dir: str = DEFAULT_WORKSPACE_DIR):
        self.host_dir = Path(host_dir)
        self.workspace_dir = Path(workspace_dir)
        self.log_file = Path(DEFAULT_LOG_FILE)
        self.max_retries = MAX_RETRIES

    def validate_permissions(self, directory: Path, operation: str) -> bool:
        """Validate directory permissions for read/write operations."""
        if not directory.exists():
            logger.error(f"Directory {directory} does not exist")
            return False

        if operation == "read" and not os.access(directory, os.R_OK):
            logger.error(f"No read permission for {directory}")
            sys.exit(PERMISSION_ERROR)

        if operation == "write" and not os.access(directory, os.W_OK):
            logger.error(f"No write permission for {directory}")
            sys.exit(PERMISSION_ERROR)

        return True

    def check_readonly_filesystem(self, directory: Path) -> bool:
        """Check if filesystem is read-only."""
        test_file = directory / ".write_test"
        try:
            test_file.touch()
            test_file.unlink()
            return False
        except OSError:
            return True

    def build_rsync_command(self, source: Path, dest: Path, dry_run: bool = False, use_gitignore: bool = True, sync_direction: str = "host_to_workspace") -> list[str]:
        """Build rsync command with appropriate options and exclusions based on sync direction."""
        cmd = [
            "rsync",
            "-av",
            "--stats",
            "--human-readable",
            "--delete",
        ]

        # Choose exclusions based on sync direction
        if sync_direction == "workspace_to_host":
            # Container -> Host: exclude .git to prevent syncing back
            exclusions = RSYNC_EXCLUSIONS_TO_HOST
            logger.debug("Using container->host exclusions (includes .git)")
        elif sync_direction == "host_to_workspace":
            # Host -> Container: allow .git to be synced
            exclusions = RSYNC_EXCLUSIONS_COMMON
            logger.debug("Using host->container exclusions (allows .git)")
        else:
            # Default to host->container behavior for any other case
            exclusions = RSYNC_EXCLUSIONS_COMMON
            logger.debug(f"Unknown sync direction '{sync_direction}', defaulting to host->container exclusions")

        # Add exclusions
        for exclusion in exclusions:
            cmd.append(f"--exclude={exclusion}")

        if dry_run:
            cmd.append("--dry-run")

        # Add .gitignore exclusions if file exists
        gitignore_path = source / ".gitignore"
        if use_gitignore and gitignore_path.exists():
            cmd.append(f"--exclude-from={gitignore_path}")
            logger.info("Using .gitignore filters")

        # Add source and destination (with trailing slashes for proper sync)
        cmd.extend([f"{source}/", f"{dest}/"])

        return cmd

    def run_rsync(self, cmd: list[str], operation: str) -> tuple[bool, int]:
        """Execute rsync command with retry logic."""
        for attempt in range(1, self.max_retries + 1):
            try:
                logger.debug(f"Running command: {' '.join(cmd)}")
                result = subprocess.run(cmd, capture_output=True, text=True, check=False)

                # Log output
                if result.stdout:
                    for line in result.stdout.strip().split("\n"):
                        if line:
                            logger.debug(line)

                if result.stderr:
                    for line in result.stderr.strip().split("\n"):
                        if line:
                            logger.warning(line)

                if result.returncode == 0:
                    # Parse file count from rsync stats
                    file_count = 0
                    for line in result.stdout.split("\n"):
                        if "Number of files transferred" in line or "Number of regular files transferred" in line:
                            with contextlib.suppress(IndexError, ValueError):
                                file_count = int("".join(filter(str.isdigit, line.split(":")[1])))

                    logger.info(f"{operation} completed successfully. Transferred {file_count} files")
                    return True, file_count
                else:
                    logger.warning(f"Rsync attempt {attempt} failed with code {result.returncode}")
                    if attempt < self.max_retries:
                        time.sleep(RETRY_DELAY_SECONDS)

            except Exception as e:
                logger.error(f"Error running rsync: {e}")
                if attempt < self.max_retries:
                    time.sleep(RETRY_DELAY_SECONDS)

        logger.error(f"Failed to sync after {self.max_retries} attempts")
        return False, -1

    def sync_host_to_workspace(self) -> int:
        """Sync from /host to /workspace (initial sync on container startup)."""
        if not self.host_dir.exists() or not any(self.host_dir.iterdir()):
            logger.info("No host directory found or empty, skipping sync")
            return 0

        logger.info("Starting host to workspace sync...")

        # Validate permissions
        if not self.validate_permissions(self.host_dir, "read"):
            return PERMISSION_ERROR
        if not self.validate_permissions(self.workspace_dir, "write"):
            return PERMISSION_ERROR

        # Build and run rsync command (host->workspace allows .git)
        cmd = self.build_rsync_command(self.host_dir, self.workspace_dir, sync_direction="host_to_workspace")
        success, file_count = self.run_rsync(cmd, "Host sync")

        return 0 if success else SYNC_ERROR

    def sync_workspace_to_host(self, dry_run: bool = False) -> int:
        """Sync from /workspace to /host (manual sync back to host)."""
        if not self.workspace_dir.exists() or not any(self.workspace_dir.iterdir()):
            logger.info("No workspace directory found or empty, skipping reverse sync")
            return 0

        if dry_run:
            logger.info("Running dry-run sync (no changes will be made)")
        else:
            logger.info("Syncing workspace changes back to host...")

        # Validate permissions
        if not self.validate_permissions(self.workspace_dir, "read"):
            return PERMISSION_ERROR
        if not self.validate_permissions(self.host_dir, "write"):
            return PERMISSION_ERROR

        # Check if host is read-only
        if not dry_run and self.check_readonly_filesystem(self.host_dir):
            logger.error("Host filesystem appears to be read-only")
            return READONLY_ERROR

        # Build and run rsync command (workspace->host excludes .git)
        cmd = self.build_rsync_command(self.workspace_dir, self.host_dir, dry_run=dry_run, sync_direction="workspace_to_host")
        operation = "Dry-run" if dry_run else "Workspace to host sync"
        success, file_count = self.run_rsync(cmd, operation)

        if dry_run and success:
            logger.info("Dry-run completed - no changes made")

        return 0 if success else SYNC_ERROR

    def configure_code_server(self):
        """Configure code-server settings."""
        config_dir = Path("/home/coder/.config/code-server")
        config_dir.mkdir(parents=True, exist_ok=True)

        config_file = config_dir / "config.yaml"
        config_file.write_text(CODE_SERVER_CONFIG)
        logger.info("Code-server configuration created")

        # Fix permissions
        try:
            subprocess.run(["chown", "-R", "coder:coder", "/home/coder/.config"], check=False)
        except Exception as e:
            logger.warning(f"Could not change ownership of /home/coder/.config: {e}")

        if self.workspace_dir.exists():
            try:
                subprocess.run(["chown", "-R", "coder:coder", str(self.workspace_dir)], check=False)
            except Exception as e:
                logger.warning(f"Could not change ownership of {self.workspace_dir}: {e}")


def main():
    """Main entry point for container sync functionality."""
    parser = argparse.ArgumentParser(description="CLUD container sync utility")
    parser.add_argument("command", choices=["init", "sync", "sync-preview", "sync-status"], help="Command to execute")
    parser.add_argument("--host-dir", default=DEFAULT_HOST_DIR, help=f"Host directory path (default: {DEFAULT_HOST_DIR})")
    parser.add_argument("--workspace-dir", default=DEFAULT_WORKSPACE_DIR, help=f"Workspace directory path (default: {DEFAULT_WORKSPACE_DIR})")
    parser.add_argument("--verbose", action="store_true", help="Enable verbose logging")

    args = parser.parse_args()

    if args.verbose:
        logger.setLevel(logging.DEBUG)

    # Set up Anthropic API key if provided
    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if api_key:
        logger.info("Anthropic API key configured")

    # Initialize sync handler
    sync = ContainerSync(args.host_dir, args.workspace_dir)

    # Execute command
    if args.command == "init":
        # Initial sync and setup
        exit_code = sync.sync_host_to_workspace()
        if exit_code == 0:
            sync.configure_code_server()
        return exit_code

    elif args.command == "sync":
        # Sync workspace back to host
        return sync.sync_workspace_to_host(dry_run=False)

    elif args.command == "sync-preview" or args.command == "sync-status":
        # Dry-run sync to preview changes
        return sync.sync_workspace_to_host(dry_run=True)

    return 0


if __name__ == "__main__":
    sys.exit(main())
