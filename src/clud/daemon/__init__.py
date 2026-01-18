"""Playwright multi-terminal daemon module for clud.

This module provides a multi-terminal UI via Playwright browser with 8 xterm.js
terminals in a flex grid layout. Each terminal runs an independent shell session.
"""

# Re-export DaemonInfo from playwright_daemon to avoid circular imports
from clud.daemon.playwright_daemon import DaemonInfo


class Daemon:
    """Proxy class for multi-terminal daemon operations with lazy-loaded implementation."""

    @staticmethod
    async def start(num_terminals: int = 8) -> DaemonInfo:
        """Start the multi-terminal daemon with Playwright browser.

        This is an async method that launches the server and browser.

        Args:
            num_terminals: Number of terminals to create (default 8)

        Returns:
            DaemonInfo with daemon process details
        """
        from clud.daemon.playwright_daemon import PlaywrightDaemon

        daemon = PlaywrightDaemon(num_terminals=num_terminals)
        return await daemon.start()

    @staticmethod
    def is_running() -> bool:
        """Check if the daemon is currently running.

        Returns:
            True if daemon is running, False otherwise
        """
        from clud.daemon.playwright_daemon import PlaywrightDaemon

        return PlaywrightDaemon.is_running()


__all__ = [
    "Daemon",
    "DaemonInfo",
]
