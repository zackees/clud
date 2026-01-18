"""CLI handler for the multi-terminal daemon command.

This module provides the command handler for `clud --daemon`, which launches
a Playwright browser with multiple xterm.js terminals in a grid layout.
"""

from __future__ import annotations

import asyncio
import logging
import sys
from pathlib import Path

logger = logging.getLogger(__name__)


def handle_daemon_command(num_terminals: int = 8) -> int:
    """Handle the --daemon command to launch multi-terminal UI.

    Launches a Playwright browser with multiple xterm.js terminals.
    Each terminal starts in the user's home directory.

    Args:
        num_terminals: Number of terminals to create (default 8)

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    home_dir = Path.home()

    print("Starting CLUD multi-terminal daemon...")
    print(f"  {num_terminals} terminals will open in Playwright browser")
    print(f"  All terminals start in: {home_dir}")
    print()

    try:
        # Run the async daemon in the event loop
        return asyncio.run(_run_daemon_async(num_terminals))
    except KeyboardInterrupt:
        print("\nDaemon interrupted by user.")
        return 0
    except Exception as e:
        logger.exception("Daemon failed to start")
        print(f"Error: Failed to start daemon: {e}", file=sys.stderr)
        return 1


async def _run_daemon_async(num_terminals: int) -> int:
    """Run the daemon asynchronously.

    Args:
        num_terminals: Number of terminals to create

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    from clud.daemon.playwright_daemon import PlaywrightDaemon

    daemon = PlaywrightDaemon(num_terminals=num_terminals)

    try:
        info = await daemon.start()
        print(f"Daemon started (PID: {info.pid}, Port: {info.port})")
        print("Close the browser window to stop the daemon.")
        print()

        # Wait for user to close the browser
        await daemon.wait_for_close()

        print("\nBrowser closed. Shutting down daemon...")
        return 0

    finally:
        # Ensure cleanup happens
        await daemon.close()
