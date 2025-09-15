#!/usr/bin/env python3
"""Background agent for continuous workspace synchronization."""

import argparse
import asyncio
import logging
import signal
import sys
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from clud.container_sync import ContainerSync

# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format="[%(asctime)s] [%(levelname)s] [bg-agent] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger(__name__)


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
