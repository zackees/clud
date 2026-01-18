"""End-to-end tests for the Playwright multi-terminal daemon.

This test validates that the daemon server functions correctly with terminals.
Browser-based tests are skipped in CI environments where Chromium may not work.
Run with: bash test --full
"""

# Playwright has incomplete type stubs - disable type checking for E2E tests
# pyright: reportMissingImports=false, reportUnknownVariableType=false, reportUnknownMemberType=false, reportUnknownParameterType=false, reportUnknownArgumentType=false, reportOptionalMemberAccess=false

import asyncio
import logging
import os
import platform
import time
import unittest
from pathlib import Path

from playwright.async_api import Page as AsyncPage
from playwright.async_api import async_playwright
from playwright.sync_api import Page

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# Skip browser tests in CI environments
SKIP_BROWSER_TESTS = os.environ.get("CI", "").lower() in ("true", "1", "yes")


class TestDaemonE2E(unittest.TestCase):
    """End-to-end tests for the multi-terminal daemon."""

    # Use unique port range for this test (8950-8960) to avoid conflicts
    port_start: int = 8950
    port_end: int = 8960

    def test_daemon_server_starts_and_serves_html(self) -> None:
        """Test that DaemonServer starts and serves the HTML page."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=4)
            try:
                http_port, ws_port = await server.start()

                logger.info("Server started on HTTP port %d, WS port %d", http_port, ws_port)

                # Verify ports are in expected range
                self.assertGreaterEqual(http_port, 8000)
                self.assertLess(http_port, 9000)
                self.assertGreaterEqual(ws_port, http_port + 1)

                # Verify server is running
                self.assertTrue(server.is_running())

                # Fetch the HTML page using httpx
                import httpx

                async with httpx.AsyncClient() as client:
                    response = await client.get(f"http://localhost:{http_port}/")
                    self.assertEqual(response.status_code, 200)
                    self.assertIn("CLUD Multi-Terminal", response.text)
                    self.assertIn("terminal-0", response.text)
                    self.assertIn("terminal-3", response.text)
                    # Should have WebSocket URLs
                    self.assertIn(f"ws://localhost:{ws_port}/ws/0", response.text)

            finally:
                await server.stop()

        asyncio.run(run_test())

    def test_daemon_server_terminal_manager_starts(self) -> None:
        """Test that DaemonServer creates and starts terminal manager."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=2)
            try:
                await server.start()

                # Verify terminal manager was created
                self.assertIsNotNone(server.terminal_manager)

                # Verify terminals were started
                if server.terminal_manager:
                    count = server.terminal_manager.get_running_count()
                    logger.info("Running terminals: %d", count)
                    # At least one terminal should be running (may not be all on all systems)
                    self.assertGreaterEqual(count, 1)

            finally:
                await server.stop()

        asyncio.run(run_test())

    def test_daemon_server_clean_shutdown(self) -> None:
        """Test that DaemonServer shuts down cleanly."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=2)
            await server.start()

            # Verify running
            self.assertTrue(server.is_running())

            # Stop the server
            await server.stop()

            # Verify stopped
            self.assertFalse(server.is_running())
            self.assertIsNone(server.terminal_manager)

        asyncio.run(run_test())

    @unittest.skipIf(SKIP_BROWSER_TESTS, "Skipping browser tests in CI environment")
    def test_playwright_browser_opens_with_terminals(self) -> None:
        """Test that Playwright browser opens and displays terminals."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=4)
            try:
                http_port, _ws_port = await server.start()
                server_url = f"http://localhost:{http_port}/"
                logger.info("Server running at %s", server_url)

                # Use Playwright async API to keep in same event loop
                async with async_playwright() as playwright:
                    browser = await playwright.chromium.launch(headless=True)
                    context = await browser.new_context()
                    page = await context.new_page()

                    # Navigate to the daemon page
                    logger.info("Navigating to %s", server_url)
                    await page.goto(server_url, wait_until="networkidle", timeout=15000)

                    # Verify page title
                    title = await page.title()
                    logger.info("Page title: %s", title)
                    self.assertIn("CLUD", title)

                    # Verify all 4 terminals are present
                    for i in range(4):
                        terminal_div = await page.query_selector(f"#terminal-{i}")
                        self.assertIsNotNone(terminal_div, f"Terminal {i} should be present on the page")

                    # Verify container exists (uses .container class in template)
                    container = await page.query_selector(".container")
                    self.assertIsNotNone(container, "Terminal container should exist")

                    # Wait a moment for xterm.js to initialize
                    await asyncio.sleep(2)

                    # Check that xterm terminal instances are created
                    xterm_elements = await page.query_selector_all(".xterm")
                    logger.info("Found %d xterm elements", len(xterm_elements))
                    # At least some terminals should have initialized
                    self.assertGreaterEqual(len(xterm_elements), 1, "At least one xterm instance should be created")

                    # Take a screenshot for debugging
                    screenshot_path = Path(__file__).parent / "artifacts" / "test_daemon_terminals.png"
                    screenshot_path.parent.mkdir(parents=True, exist_ok=True)
                    await page.screenshot(path=str(screenshot_path))
                    logger.info("Screenshot saved to %s", screenshot_path)

                    await browser.close()

            finally:
                await server.stop()

        asyncio.run(run_test())

    @unittest.skipIf(SKIP_BROWSER_TESTS, "Skipping browser tests in CI environment")
    def test_terminal_receives_shell_prompt(self) -> None:
        """Test that terminals receive a shell prompt after initialization."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=1)
            try:
                http_port, _ws_port = await server.start()
                server_url = f"http://localhost:{http_port}/"

                async with async_playwright() as playwright:
                    browser = await playwright.chromium.launch(headless=True)
                    context = await browser.new_context()
                    page = await context.new_page()

                    # Navigate to the daemon page
                    await page.goto(server_url, wait_until="networkidle", timeout=15000)

                    # Wait for xterm to initialize
                    await page.wait_for_selector(".xterm", timeout=10000)

                    # Give time for shell to initialize and send prompt
                    await asyncio.sleep(3)

                    # Get terminal content
                    terminal_text = await self._get_terminal_text_async(page)
                    logger.info("Terminal text length: %d", len(terminal_text))
                    logger.info("Terminal text (first 200 chars): %s", terminal_text[:200] if terminal_text else "empty")

                    # Terminal should have some content (prompt or initial output)
                    # Different systems have different prompts, so just check for some content
                    self.assertGreater(len(terminal_text.strip()), 0, "Terminal should have some content after initialization")

                    await browser.close()

            finally:
                await server.stop()

        asyncio.run(run_test())

    @unittest.skipIf(SKIP_BROWSER_TESTS, "Skipping browser tests in CI environment")
    def test_terminal_executes_command(self) -> None:
        """Test that a terminal can execute a simple command."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=1)
            try:
                http_port, _ws_port = await server.start()
                server_url = f"http://localhost:{http_port}/"

                async with async_playwright() as playwright:
                    browser = await playwright.chromium.launch(headless=True)
                    context = await browser.new_context()
                    page = await context.new_page()

                    # Navigate to the daemon page
                    await page.goto(server_url, wait_until="networkidle", timeout=15000)

                    # Wait for xterm to initialize
                    await page.wait_for_selector(".xterm", timeout=10000)

                    # Give time for shell to initialize
                    await asyncio.sleep(3)

                    # Type a unique echo command
                    logger.info("Typing echo command...")
                    await self._type_in_terminal_async(page, "echo DAEMON_TEST_12345\r")

                    # Wait for command to execute
                    await asyncio.sleep(2)

                    # Get terminal content
                    terminal_text = await self._get_terminal_text_async(page)
                    logger.info("Terminal text after command: %s", terminal_text[-300:] if terminal_text else "empty")

                    # The echo output should appear in the terminal
                    self.assertIn("DAEMON_TEST_12345", terminal_text, "Echo command output should appear in terminal")

                    await browser.close()

            finally:
                await server.stop()

        asyncio.run(run_test())

    @unittest.skipIf(SKIP_BROWSER_TESTS, "Skipping browser tests in CI environment")
    def test_terminals_start_in_home_directory(self) -> None:
        """Test that terminals start in the user's home directory."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=1)
            try:
                http_port, _ws_port = await server.start()
                server_url = f"http://localhost:{http_port}/"

                async with async_playwright() as playwright:
                    browser = await playwright.chromium.launch(headless=True)
                    context = await browser.new_context()
                    page = await context.new_page()

                    # Navigate to the daemon page
                    await page.goto(server_url, wait_until="networkidle", timeout=15000)

                    # Wait for xterm to initialize
                    await page.wait_for_selector(".xterm", timeout=10000)

                    # Give time for shell to initialize
                    await asyncio.sleep(3)

                    # Execute pwd command
                    logger.info("Typing pwd command...")
                    await self._type_in_terminal_async(page, "pwd\r")

                    # Wait for command to execute
                    await asyncio.sleep(2)

                    # Get terminal content
                    terminal_text = await self._get_terminal_text_async(page)
                    logger.info("Terminal text after pwd: %s", terminal_text[-300:] if terminal_text else "empty")

                    # Verify the path matches home directory
                    home_dir = str(Path.home())
                    # On Windows, paths may use backslashes or forward slashes
                    # Also, git-bash uses /c/Users/... format
                    if platform.system() == "Windows":
                        # Check for various Windows home path formats
                        home_check = (
                            home_dir.replace("\\", "/") in terminal_text or home_dir in terminal_text or "/c/Users/" in terminal_text.lower() or "~" in terminal_text  # Some shells show ~
                        )
                    else:
                        home_check = home_dir in terminal_text or "~" in terminal_text

                    self.assertTrue(home_check, f"pwd should show home directory. Expected {home_dir}, got: {terminal_text[-300:]}")

                    await browser.close()

            finally:
                await server.stop()

        asyncio.run(run_test())

    def test_daemon_shutdown_no_zombie_processes(self) -> None:
        """Test that daemon shutdown doesn't leave zombie processes."""
        import subprocess

        from clud.daemon.server import DaemonServer

        # Get initial process count (platform-specific)
        def get_child_process_count() -> int:
            """Get count of child bash/sh processes."""
            if platform.system() == "Windows":
                # On Windows, check for bash.exe and sh.exe
                result = subprocess.run(
                    ["tasklist", "/FI", "IMAGENAME eq bash.exe"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                bash_count = result.stdout.count("bash.exe")
                result = subprocess.run(
                    ["tasklist", "/FI", "IMAGENAME eq sh.exe"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                sh_count = result.stdout.count("sh.exe")
                return bash_count + sh_count
            else:
                # On Unix, use ps to count shell processes
                result = subprocess.run(
                    ["ps", "-o", "pid,ppid,comm", "-A"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                # Count lines containing bash or sh
                lines = result.stdout.split("\n")
                count = sum(1 for line in lines if "bash" in line or "/sh" in line)
                return count

        # Get initial count
        initial_count = get_child_process_count()
        logger.info("Initial shell process count: %d", initial_count)

        async def run_test() -> None:
            server = DaemonServer(num_terminals=2)
            await server.start()
            logger.info("Server started with 2 terminals")

            # Give time for terminals to start
            await asyncio.sleep(2)

            count_with_terminals = get_child_process_count()
            logger.info("Shell process count with terminals: %d", count_with_terminals)

            # Stop server (should clean up terminals)
            await server.stop()
            logger.info("Server stopped")

        asyncio.run(run_test())

        # Give time for cleanup
        time.sleep(2)

        final_count = get_child_process_count()
        logger.info("Final shell process count: %d", final_count)

        # After cleanup, process count should be back to initial (or close to it)
        # Allow for some variance due to other system processes
        self.assertLessEqual(
            final_count,
            initial_count + 2,  # Allow small variance
            f"Process count should return close to initial after cleanup. Initial: {initial_count}, Final: {final_count}",
        )

    def _type_in_terminal(self, page: Page, text: str) -> None:
        """Type text into the terminal by focusing and typing (sync version)."""
        # For our daemon, we target the first terminal's xterm
        terminal_selector = "#terminal-0 .xterm"
        try:
            # Click to focus
            page.click(terminal_selector, timeout=2000)
        except Exception:
            # Try fallback selector
            page.click(".xterm")

        # Type the text
        page.keyboard.type(text)

    async def _type_in_terminal_async(self, page: AsyncPage, text: str) -> None:
        """Type text into the terminal by focusing and typing (async version)."""
        # For our daemon, we target the first terminal's xterm
        terminal_selector = "#terminal-0 .xterm"
        try:
            # Click to focus
            await page.click(terminal_selector, timeout=2000)
        except Exception:
            # Try fallback selector
            await page.click(".xterm")

        # Type the text
        await page.keyboard.type(text)

    def _get_terminal_text(self, page: Page) -> str:
        """Extract visible text from the terminal (sync version)."""
        try:
            # Use JavaScript to get terminal text content
            text = page.evaluate("""() => {
                // Try to get rows with content from first terminal
                let terminal = document.querySelector('#terminal-0');
                if (!terminal) terminal = document.querySelector('.terminal-wrapper');
                if (!terminal) terminal = document.body;

                let rows = terminal.querySelectorAll('.xterm-rows .xterm-row');
                if (rows.length === 0) {
                    // Try alternative selector
                    rows = terminal.querySelectorAll('.xterm .xterm-screen .xterm-rows > div');
                }
                if (rows.length === 0) {
                    // Get any text content from xterm
                    const xtermElement = terminal.querySelector('.xterm');
                    return xtermElement ? xtermElement.innerText : '';
                }
                return Array.from(rows).map(row => row.textContent || '').join('\\n');
            }""")
            return str(text) if text else ""
        except Exception as e:
            logger.warning("Could not extract terminal text: %s", e)
            return ""

    async def _get_terminal_text_async(self, page: AsyncPage) -> str:
        """Extract visible text from the terminal (async version)."""
        try:
            # Use JavaScript to get terminal text content
            text = await page.evaluate("""() => {
                // Try to get rows with content from first terminal
                let terminal = document.querySelector('#terminal-0');
                if (!terminal) terminal = document.querySelector('.terminal-wrapper');
                if (!terminal) terminal = document.body;

                let rows = terminal.querySelectorAll('.xterm-rows .xterm-row');
                if (rows.length === 0) {
                    // Try alternative selector
                    rows = terminal.querySelectorAll('.xterm .xterm-screen .xterm-rows > div');
                }
                if (rows.length === 0) {
                    // Get any text content from xterm
                    const xtermElement = terminal.querySelector('.xterm');
                    return xtermElement ? xtermElement.innerText : '';
                }
                return Array.from(rows).map(row => row.textContent || '').join('\\n');
            }""")
            return str(text) if text else ""
        except Exception as e:
            logger.warning("Could not extract terminal text: %s", e)
            return ""


if __name__ == "__main__":
    unittest.main()
