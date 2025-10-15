# pyright: reportUnknownVariableType=false, reportUnknownMemberType=false, reportUnknownArgumentType=false, reportUnknownParameterType=false, reportMissingImports=false
"""End-to-end tests for Terminal tab in the main Web UI using Playwright.

This test validates that the terminal tab works correctly when switching
between tabs in the main Web UI interface.
Run with: bash test --full
"""

import logging
import os
import subprocess
import time
import unittest
from pathlib import Path

from playwright.sync_api import ConsoleMessage, Page, sync_playwright

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class TestWebUITerminalTabE2E(unittest.TestCase):
    """End-to-end tests for Terminal tab in the main Web UI."""

    server_process: subprocess.Popen[bytes] | None = None
    server_url: str = "http://localhost:8901"
    startup_timeout: int = 30  # seconds

    @classmethod
    def setUpClass(cls) -> None:
        """Start the Web UI server before running tests."""
        logger.info("Starting Web UI server for terminal tab e2e tests...")

        # Start the server using the actual CLI command
        # Use port 8901 to avoid conflicts with other tests
        env = os.environ.copy()
        # Prevent browser from auto-opening during tests
        env["CLUD_NO_BROWSER"] = "1"

        cls.server_process = subprocess.Popen(
            ["uv", "run", "--no-sync", "clud", "--webui", "8901"],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(Path(__file__).parent.parent),
        )

        # Wait for server to be ready
        start_time = time.time()
        server_ready = False

        while time.time() - start_time < cls.startup_timeout:
            try:
                import httpx

                response = httpx.get(f"{cls.server_url}/health", timeout=1.0)
                if response.status_code == 200:
                    server_ready = True
                    logger.info("Web UI server is ready")
                    break
            except Exception:
                time.sleep(0.5)

        if not server_ready:
            cls.tearDownClass()
            raise RuntimeError(f"Web UI server failed to start within {cls.startup_timeout} seconds")

    @classmethod
    def tearDownClass(cls) -> None:
        """Stop the Web UI server after all tests."""
        if cls.server_process:
            logger.info("Stopping Web UI server...")
            cls.server_process.terminate()
            try:
                cls.server_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                logger.warning("Server did not terminate gracefully, killing...")
                cls.server_process.kill()
                cls.server_process.wait()

    def test_terminal_tab_switching_works(self) -> None:
        """Test that switching to terminal tab works correctly without flashing/overwriting."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                # Filter out known harmless errors
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Listen for console errors
            page.on("console", on_console_message)

            # Navigate to main page
            logger.info("Navigating to %s", self.server_url)
            page.goto(self.server_url, wait_until="networkidle", timeout=10000)

            # Wait for page to be visible
            logger.info("Waiting for app to load...")
            page.wait_for_selector(".app-container", timeout=5000)

            # Verify we start on chat tab (default)
            chat_tab = page.query_selector(".tab-button.active")
            self.assertIsNotNone(chat_tab, "Should have an active tab on load")
            chat_tab_text = chat_tab.text_content() if chat_tab else ""
            logger.info("Initial active tab: %s", chat_tab_text)

            # Take initial screenshot
            screenshot_path = Path(__file__).parent / "artifacts" / "test_webui_terminal_tab_before.png"
            screenshot_path.parent.mkdir(parents=True, exist_ok=True)
            page.screenshot(path=str(screenshot_path))
            logger.info("Before screenshot saved to %s", screenshot_path)

            # Click on Terminal tab
            logger.info("Clicking Terminal tab...")
            terminal_tab_button = page.query_selector(".tab-button:has-text('Terminal')")
            self.assertIsNotNone(terminal_tab_button, "Terminal tab button should exist")
            if terminal_tab_button:
                terminal_tab_button.click()

            # Wait for terminal to become visible
            time.sleep(2)

            # Verify Terminal tab is now active
            active_tab = page.query_selector(".tab-button.active")
            self.assertIsNotNone(active_tab, "Should have an active tab after clicking")
            active_tab_text = active_tab.text_content() if active_tab else ""
            logger.info("Active tab after click: %s", active_tab_text)
            self.assertEqual(active_tab_text.strip(), "Terminal", "Terminal tab should be active")

            # Wait for terminal to initialize
            logger.info("Waiting for terminal to initialize...")
            page.wait_for_selector(".xterm", timeout=10000)
            time.sleep(2)

            # Take screenshot after tab switch
            screenshot_path_after = Path(__file__).parent / "artifacts" / "test_webui_terminal_tab_after_switch.png"
            page.screenshot(path=str(screenshot_path_after))
            logger.info("After switch screenshot saved to %s", screenshot_path_after)

            # Type a test command
            logger.info("Typing 'echo TERMINAL_TAB_TEST' command...")
            self._type_in_terminal(page, "echo TERMINAL_TAB_TEST\r")
            time.sleep(1)

            # Get terminal output
            terminal_text = self._get_terminal_text(page)
            logger.info("Terminal output length: %d characters", len(terminal_text))
            logger.info("Terminal output sample: %s", terminal_text[:500])

            # Verify command output is visible
            self.assertIn("TERMINAL_TAB_TEST", terminal_text, "Terminal should show command output")

            # Take final screenshot
            screenshot_path_final = Path(__file__).parent / "artifacts" / "test_webui_terminal_tab_final.png"
            page.screenshot(path=str(screenshot_path_final))
            logger.info("Final screenshot saved to %s", screenshot_path_final)

            # Verify no critical console errors
            if console_errors:
                logger.warning("Console errors detected:")
                for error in console_errors:
                    logger.warning("  - %s", error)

            self.assertEqual(len(console_errors), 0, f"Expected no console errors, but found {len(console_errors)}: {console_errors}")

            browser.close()

    def test_terminal_tab_multiple_switches(self) -> None:
        """Test that switching between tabs multiple times doesn't break terminal."""
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Navigate to main page
            logger.info("Navigating to %s", self.server_url)
            page.goto(self.server_url, wait_until="networkidle", timeout=10000)

            # Wait for page to be visible
            page.wait_for_selector(".app-container", timeout=5000)

            # Switch to Terminal tab
            logger.info("Switching to Terminal tab (1st time)...")
            terminal_tab = page.query_selector(".tab-button:has-text('Terminal')")
            if terminal_tab:
                terminal_tab.click()
            time.sleep(2)
            page.wait_for_selector(".xterm", timeout=10000)

            # Type first command
            logger.info("Typing first command...")
            self._type_in_terminal(page, "echo FIRST_COMMAND\r")
            time.sleep(1)

            # Switch to Chat tab
            logger.info("Switching to Chat tab...")
            chat_tab = page.query_selector(".tab-button:has-text('Chat')")
            if chat_tab:
                chat_tab.click()
            time.sleep(1)

            # Switch back to Terminal tab
            logger.info("Switching back to Terminal tab (2nd time)...")
            terminal_tab = page.query_selector(".tab-button:has-text('Terminal')")
            if terminal_tab:
                terminal_tab.click()
            time.sleep(2)

            # Type second command
            logger.info("Typing second command...")
            self._type_in_terminal(page, "echo SECOND_COMMAND\r")
            time.sleep(1)

            # Get terminal output
            terminal_text = self._get_terminal_text(page)
            logger.info("Terminal output after multiple switches: %s", terminal_text[-500:])

            # Both commands should be visible
            self.assertIn("FIRST_COMMAND", terminal_text, "First command should still be visible")
            self.assertIn("SECOND_COMMAND", terminal_text, "Second command should be visible")

            # Switch away and back one more time
            logger.info("Switching to Settings tab...")
            settings_tab = page.query_selector(".tab-button:has-text('Settings')")
            if settings_tab:
                settings_tab.click()
            time.sleep(1)

            logger.info("Switching back to Terminal tab (3rd time)...")
            terminal_tab = page.query_selector(".tab-button:has-text('Terminal')")
            if terminal_tab:
                terminal_tab.click()
            time.sleep(2)

            # Type third command
            logger.info("Typing third command...")
            self._type_in_terminal(page, "echo THIRD_COMMAND\r")
            time.sleep(1)

            # Get final terminal output
            terminal_text = self._get_terminal_text(page)
            logger.info("Final terminal output: %s", terminal_text[-500:])

            # All three commands should be visible
            self.assertIn("FIRST_COMMAND", terminal_text, "First command should still be visible")
            self.assertIn("SECOND_COMMAND", terminal_text, "Second command should still be visible")
            self.assertIn("THIRD_COMMAND", terminal_text, "Third command should be visible")

            browser.close()

    def test_terminal_tab_new_terminal_button(self) -> None:
        """Test that creating a new terminal in the Terminal tab works correctly."""
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Navigate to main page
            logger.info("Navigating to %s", self.server_url)
            page.goto(self.server_url, wait_until="networkidle", timeout=10000)

            # Wait for page to be visible
            page.wait_for_selector(".app-container", timeout=5000)

            # Switch to Terminal tab
            logger.info("Switching to Terminal tab...")
            terminal_tab = page.query_selector(".tab-button:has-text('Terminal')")
            if terminal_tab:
                terminal_tab.click()
            time.sleep(2)
            page.wait_for_selector(".xterm", timeout=10000)

            # Verify we start with one terminal tab
            terminal_tabs = page.query_selector_all(".terminal-tab")
            logger.info("Initial number of terminal tabs: %d", len(terminal_tabs))
            self.assertEqual(len(terminal_tabs), 1, "Should start with one terminal tab")

            # Click the "+" button to create a new terminal
            logger.info("Clicking new terminal button...")
            new_terminal_btn = page.query_selector(".new-terminal-btn")
            self.assertIsNotNone(new_terminal_btn, "New terminal button should exist")
            if new_terminal_btn:
                new_terminal_btn.click()
            time.sleep(2)

            # Verify we now have two terminal tabs
            terminal_tabs = page.query_selector_all(".terminal-tab")
            logger.info("Number of terminal tabs after creating new: %d", len(terminal_tabs))
            self.assertEqual(len(terminal_tabs), 2, "Should have two terminal tabs after clicking +")

            # Type command in second terminal
            logger.info("Typing command in second terminal...")
            self._type_in_terminal(page, "echo SECOND_TERMINAL\r")
            time.sleep(1)

            # Get terminal output
            terminal_text = self._get_terminal_text(page)
            logger.info("Terminal output: %s", terminal_text[-300:])
            self.assertIn("SECOND_TERMINAL", terminal_text, "Second terminal should show command output")

            # Switch to first terminal
            logger.info("Switching to first terminal tab...")
            first_terminal_tab = terminal_tabs[0]
            first_terminal_tab.click()
            time.sleep(1)

            # Type command in first terminal
            logger.info("Typing command in first terminal...")
            self._type_in_terminal(page, "echo FIRST_TERMINAL\r")
            time.sleep(1)

            # Get terminal output from first terminal
            terminal_text_first = self._get_terminal_text(page)
            logger.info("First terminal output: %s", terminal_text_first[-300:])
            self.assertIn("FIRST_TERMINAL", terminal_text_first, "First terminal should show command output")

            browser.close()

    def _type_in_terminal(self, page: Page, text: str) -> None:
        """Type text into the terminal by focusing and typing."""
        # Click on the active terminal to focus it
        terminal_selector = ".xterm-wrapper.active .xterm-helper-textarea"
        try:
            page.click(terminal_selector, timeout=1000)
        except Exception:
            # If helper textarea is not found, click on active terminal screen
            try:
                page.click(".xterm-wrapper.active .xterm-screen", timeout=1000)
            except Exception:
                # Last resort: click any visible xterm screen
                page.click(".xterm-screen:visible", timeout=1000)

        # Type the text
        page.keyboard.type(text)

    def _get_terminal_text(self, page: Page) -> str:
        """Extract visible text from the terminal."""
        try:
            # Use JavaScript to get terminal text content
            text = page.evaluate("""() => {
                // Try to get rows with content
                let rows = document.querySelectorAll('.xterm-rows .xterm-row');
                if (rows.length === 0) {
                    // Try alternative selector
                    rows = document.querySelectorAll('.xterm .xterm-screen .xterm-rows > div');
                }
                if (rows.length === 0) {
                    // Get any text content from xterm
                    const xtermElement = document.querySelector('.xterm');
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
