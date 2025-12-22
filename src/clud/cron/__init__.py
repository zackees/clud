"""Cron scheduler module for clud.

This module provides cron-style task scheduling functionality with cross-platform support.
"""

from dataclasses import dataclass
from typing import Literal


@dataclass
class DaemonStatus:
    """Status information for the cron daemon.

    Attributes:
        state: Current daemon state - "running", "stopped", or "stale"
        pid: Process ID if daemon has a PID file, None otherwise
    """

    state: Literal["running", "stopped", "stale"]
    pid: int | None


class Daemon:
    """Proxy class for cron daemon operations with lazy-loaded implementation."""

    @staticmethod
    def start() -> bool:
        """Start the cron daemon in the background.

        Returns:
            True if daemon started successfully, False if already running
        """
        from clud.cron.daemon import CronDaemon

        daemon = CronDaemon()
        return daemon.start()

    @staticmethod
    def stop() -> bool:
        """Stop the cron daemon gracefully.

        Returns:
            True if daemon stopped successfully, False if not running
        """
        from clud.cron.daemon import CronDaemon

        daemon = CronDaemon()
        return daemon.stop()

    @staticmethod
    def status() -> DaemonStatus:
        """Get the current daemon status.

        Returns:
            DaemonStatus object containing state and PID information
        """
        from clud.cron.daemon import CronDaemon

        daemon = CronDaemon()
        state, pid = daemon.status()
        return DaemonStatus(state=state, pid=pid)

    @staticmethod
    def is_running() -> bool:
        """Check if the daemon is currently running.

        Returns:
            True if daemon is running, False otherwise
        """
        from clud.cron.daemon import CronDaemon

        daemon = CronDaemon()
        return daemon.is_running()


__all__ = [
    "Daemon",
    "DaemonStatus",
]
