"""Playwright daemon implementation for multi-terminal UI.

This module provides the main orchestrator for the multi-terminal daemon,
using Playwright to launch a Chromium browser that displays 8 xterm.js terminals.
"""

from __future__ import annotations

import asyncio
import logging
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from playwright.async_api import Browser, BrowserContext, Page, Playwright

    from clud.daemon.server import DaemonServer

logger = logging.getLogger(__name__)

# PID file location for tracking running daemon
_PID_FILE = Path.home() / ".clud" / "daemon.pid"


@dataclass
class DaemonInfo:
    """Information about a running daemon instance.

    Attributes:
        pid: Process ID of the daemon
        port: HTTP port the daemon is listening on
        num_terminals: Number of terminal instances
    """

    pid: int
    port: int
    num_terminals: int


def _ensure_playwright_browsers() -> bool:
    """Ensure Playwright browsers are installed.

    Returns:
        True if browsers are available, False if installation failed
    """
    try:
        # Try to import playwright first
        # pylint: disable=import-outside-toplevel
        from playwright.async_api import async_playwright

        del async_playwright  # Just checking import works

        # Check if Chromium is installed by looking for the executable
        # playwright stores browsers in a platform-specific location
        # We'll try to install if launching fails
        return True
    except ImportError:
        logger.error("Playwright is not installed. Run: uv pip install playwright")
        return False


async def _install_browsers() -> bool:
    """Install Playwright browsers if missing.

    Returns:
        True if installation succeeded, False otherwise
    """
    logger.info("Installing Playwright browsers...")
    try:
        # Run playwright install chromium
        process = await asyncio.create_subprocess_exec(
            sys.executable,
            "-m",
            "playwright",
            "install",
            "chromium",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await process.communicate()

        if process.returncode == 0:
            logger.info("Playwright browsers installed successfully")
            return True
        else:
            logger.error(
                "Failed to install Playwright browsers: %s",
                stderr.decode() if stderr else "Unknown error",
            )
            return False
    except Exception as e:
        logger.error("Error installing Playwright browsers: %s", e)
        return False


class PlaywrightDaemon:
    """Playwright-based multi-terminal daemon.

    Launches a Chromium browser via Playwright with multiple xterm.js terminals
    in a grid layout. Each terminal runs an independent shell session.

    Attributes:
        num_terminals: Number of terminals to create
        server: The DaemonServer instance managing HTTP/WebSocket servers
    """

    def __init__(self, num_terminals: int = 8) -> None:
        """Initialize the Playwright daemon.

        Args:
            num_terminals: Number of terminals to create (default 8)
        """
        self.num_terminals = num_terminals
        self._port: int | None = None
        self._pid: int | None = None

        # Playwright components
        self._playwright: Playwright | None = None
        self._browser: Browser | None = None
        self._context: BrowserContext | None = None
        self._page: Page | None = None

        # Server - import at runtime to avoid circular imports
        self._server: DaemonServer | None = None

        # Event for waiting on close
        self._closed_event: asyncio.Event | None = None

    @property
    def server(self) -> DaemonServer | None:
        """Get the DaemonServer instance."""
        return self._server

    def is_closed(self) -> bool:
        """Check if the browser has been closed.

        Returns:
            True if browser is closed or not started, False otherwise
        """
        if self._closed_event is None:
            return True
        return self._closed_event.is_set()

    async def start(self, port: int | None = None) -> DaemonInfo:
        """Start the multi-terminal daemon with Playwright browser.

        Launches the HTTP/WebSocket server and opens a Chromium browser
        window displaying the terminal grid.

        Args:
            port: Optional specific port to use for HTTP server

        Returns:
            DaemonInfo with daemon process details

        Raises:
            RuntimeError: If daemon fails to start
        """
        # Ensure Playwright is available
        if not _ensure_playwright_browsers():
            raise RuntimeError("Playwright is not available")

        # Import at runtime to avoid circular imports
        # pylint: disable=import-outside-toplevel
        from playwright.async_api import async_playwright

        from clud.daemon.server import DaemonServer

        try:
            # Start the server first
            self._server = DaemonServer(num_terminals=self.num_terminals)
            http_port, ws_port = await self._server.start(port=port)
            self._port = http_port

            logger.info("Server started on HTTP port %d, WS port %d", http_port, ws_port)

            # Try to launch browser, install if needed
            self._playwright = await async_playwright().start()

            try:
                self._browser = await self._playwright.chromium.launch(
                    headless=False,
                    args=[
                        "--disable-infobars",
                        "--disable-extensions",
                    ],
                )
            except Exception as e:
                # Browser might not be installed, try to install
                logger.warning("Browser launch failed, attempting to install: %s", e)
                if await _install_browsers():
                    self._browser = await self._playwright.chromium.launch(
                        headless=False,
                        args=[
                            "--disable-infobars",
                            "--disable-extensions",
                        ],
                    )
                else:
                    raise RuntimeError("Failed to launch or install Chromium browser") from e

            # Create browser context and page
            self._context = await self._browser.new_context(
                viewport={"width": 1600, "height": 900},
            )

            self._page = await self._context.new_page()

            # Navigate to the terminal page
            url = self._server.get_url()
            logger.info("Opening browser at %s", url)
            await self._page.goto(url)

            # Set up close event
            self._closed_event = asyncio.Event()

            # Listen for page close - handler receives Page argument
            self._page.on("close", self._on_page_close)

            # Listen for browser disconnect - handler receives Browser argument
            self._browser.on("disconnected", self._on_browser_close)

            # Save PID file
            self._pid = os.getpid()
            self._write_pid_file()

            return DaemonInfo(
                pid=self._pid,
                port=http_port,
                num_terminals=self.num_terminals,
            )

        except Exception as e:
            logger.error("Failed to start daemon: %s", e)
            await self.close()
            raise RuntimeError(f"Failed to start daemon: {e}") from e

    def _on_page_close(self, page: Page) -> None:
        """Handle page close event.

        Args:
            page: The page that was closed (unused but required by Playwright)
        """
        del page  # unused
        logger.info("Browser page closed")
        if self._closed_event:
            self._closed_event.set()

    def _on_browser_close(self, browser: Browser) -> None:
        """Handle browser disconnect event.

        Args:
            browser: The browser that was disconnected (unused but required by Playwright)
        """
        del browser  # unused
        logger.info("Browser disconnected")
        if self._closed_event:
            self._closed_event.set()

    async def wait_for_close(self) -> None:
        """Wait for the user to close the browser.

        Blocks until the browser window is closed.
        """
        if self._closed_event:
            await self._closed_event.wait()

    async def close(self) -> None:
        """Close the daemon and clean up all resources."""
        logger.info("Closing daemon...")

        # Close browser
        if self._page:
            try:
                await self._page.close()
            except Exception as e:
                logger.debug("Error closing page: %s", e)
            self._page = None

        if self._context:
            try:
                await self._context.close()
            except Exception as e:
                logger.debug("Error closing context: %s", e)
            self._context = None

        if self._browser:
            try:
                await self._browser.close()
            except Exception as e:
                logger.debug("Error closing browser: %s", e)
            self._browser = None

        if self._playwright:
            try:
                await self._playwright.stop()
            except Exception as e:
                logger.debug("Error stopping playwright: %s", e)
            self._playwright = None

        # Stop server
        if self._server:
            await self._server.stop()
            self._server = None

        # Remove PID file
        self._remove_pid_file()

        logger.info("Daemon closed")

    def _write_pid_file(self) -> None:
        """Write the PID file to track running daemon."""
        try:
            _PID_FILE.parent.mkdir(parents=True, exist_ok=True)
            _PID_FILE.write_text(str(self._pid))
            logger.debug("Wrote PID file: %s", _PID_FILE)
        except Exception as e:
            logger.warning("Failed to write PID file: %s", e)

    def _remove_pid_file(self) -> None:
        """Remove the PID file."""
        try:
            if _PID_FILE.exists():
                _PID_FILE.unlink()
                logger.debug("Removed PID file: %s", _PID_FILE)
        except Exception as e:
            logger.warning("Failed to remove PID file: %s", e)

    @staticmethod
    def is_running() -> bool:
        """Check if the daemon is currently running.

        Checks for the PID file and verifies the process is alive.

        Returns:
            True if daemon is running, False otherwise
        """
        if not _PID_FILE.exists():
            return False

        try:
            pid = int(_PID_FILE.read_text().strip())

            # Check if process is alive
            if sys.platform == "win32":
                # On Windows, use tasklist
                result = subprocess.run(
                    ["tasklist", "/FI", f"PID eq {pid}"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                return str(pid) in result.stdout
            else:
                # On Unix, send signal 0 to check if process exists
                os.kill(pid, 0)
                return True

        except (ValueError, ProcessLookupError, PermissionError, FileNotFoundError):
            # PID file invalid or process not running
            return False
        except Exception as e:
            logger.debug("Error checking daemon status: %s", e)
            return False
