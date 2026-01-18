"""Single optimized E2E test for the Playwright multi-terminal daemon.

This test validates the core daemon functionality: server starts, browser opens,
terminals render, and commands execute successfully.

Run with: bash test --full
"""

# Playwright has incomplete type stubs - disable type checking for E2E tests
# pyright: reportMissingImports=false, reportUnknownVariableType=false, reportUnknownMemberType=false, reportUnknownParameterType=false, reportUnknownArgumentType=false, reportOptionalMemberAccess=false

import asyncio
import logging
import os
import unittest
from pathlib import Path

from playwright.async_api import Page as AsyncPage
from playwright.async_api import async_playwright

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# Skip browser tests in CI environments
SKIP_BROWSER_TESTS = os.environ.get("CI", "").lower() in ("true", "1", "yes")


class TestDaemonE2E(unittest.TestCase):
    """Single comprehensive E2E test for the multi-terminal daemon."""

    @unittest.skipIf(SKIP_BROWSER_TESTS, "Skipping browser tests in CI environment")
    def test_daemon_multiplex_mode_e2e(self) -> None:
        """Complete E2E test: server starts, browser shows terminals, command executes."""
        from clud.daemon.server import DaemonServer

        async def run_test() -> None:
            server = DaemonServer(num_terminals=2)  # Use 2 terminals for speed
            try:
                http_port, ws_port = await server.start()
                server_url = f"http://localhost:{http_port}/"
                logger.info("Server started on HTTP port %d, WS port %d", http_port, ws_port)

                # Verify server is running
                self.assertTrue(server.is_running())
                self.assertIsNotNone(server.terminal_manager)

                # Verify terminal manager has terminals
                if server.terminal_manager:
                    count = server.terminal_manager.get_running_count()
                    logger.info("Running terminals: %d", count)
                    self.assertGreaterEqual(count, 1)

                # Browser test
                async with async_playwright() as playwright:
                    browser = await playwright.chromium.launch(headless=True)
                    context = await browser.new_context()
                    page = await context.new_page()

                    # Navigate to the daemon page
                    logger.info("Navigating to %s", server_url)
                    await page.goto(server_url, wait_until="networkidle", timeout=15000)

                    # Verify page loads with terminals
                    title = await page.title()
                    logger.info("Page title: %s", title)
                    self.assertIn("CLUD", title)

                    # Verify terminal divs exist
                    terminal_div = await page.query_selector("#terminal-0")
                    self.assertIsNotNone(terminal_div, "Terminal 0 should be present")

                    # Wait for xterm to initialize
                    await page.wait_for_selector(".xterm", timeout=10000)
                    await asyncio.sleep(2)  # Brief wait for shell init

                    # Check xterm elements are created
                    xterm_elements = await page.query_selector_all(".xterm")
                    logger.info("Found %d xterm elements", len(xterm_elements))
                    self.assertGreaterEqual(len(xterm_elements), 1)

                    # Execute a command to verify terminal works
                    logger.info("Executing test command...")
                    await self._type_in_terminal_async(page, "echo DAEMON_E2E_OK\r")
                    await asyncio.sleep(1)

                    # Verify command output
                    terminal_text = await self._get_terminal_text_async(page)
                    logger.info("Terminal output (last 200 chars): %s", terminal_text[-200:])
                    self.assertIn("DAEMON_E2E_OK", terminal_text, "Command output should appear")

                    # Take screenshot for debugging
                    screenshot_path = Path(__file__).parent / "artifacts" / "test_daemon_e2e.png"
                    screenshot_path.parent.mkdir(parents=True, exist_ok=True)
                    await page.screenshot(path=str(screenshot_path))
                    logger.info("Screenshot saved to %s", screenshot_path)

                    await browser.close()

            finally:
                await server.stop()
                self.assertFalse(server.is_running())
                logger.info("Server stopped cleanly")

        asyncio.run(run_test())

    async def _type_in_terminal_async(self, page: AsyncPage, text: str) -> None:
        """Type text into the terminal."""
        terminal_selector = "#terminal-0 .xterm"
        try:
            await page.click(terminal_selector, timeout=2000)
        except Exception:
            await page.click(".xterm")
        await page.keyboard.type(text)

    async def _get_terminal_text_async(self, page: AsyncPage) -> str:
        """Extract visible text from the terminal."""
        try:
            text = await page.evaluate("""() => {
                let terminal = document.querySelector('#terminal-0');
                if (!terminal) terminal = document.querySelector('.terminal-wrapper');
                if (!terminal) terminal = document.body;
                let rows = terminal.querySelectorAll('.xterm-rows .xterm-row');
                if (rows.length === 0) {
                    rows = terminal.querySelectorAll('.xterm .xterm-screen .xterm-rows > div');
                }
                if (rows.length === 0) {
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
