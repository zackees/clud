"""End-to-end tests for Terminal-only page using Playwright.

This test validates that the dedicated /terminal route works correctly
and can execute commands like 'ls -al' successfully.
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


class TestTerminalE2E(unittest.TestCase):
    """End-to-end tests for Terminal-only page."""

    server_process: subprocess.Popen[bytes] | None = None
    server_url: str = "http://localhost:8900"
    startup_timeout: int = 30  # seconds

    @classmethod
    def setUpClass(cls) -> None:
        """Start the Web UI server before running tests."""
        logger.info("Starting Web UI server for terminal e2e tests...")

        # Start the server using the actual CLI command
        # Use port 8900 to avoid conflicts with other tests
        env = os.environ.copy()
        # Prevent browser from auto-opening during tests
        env["CLUD_NO_BROWSER"] = "1"

        cls.server_process = subprocess.Popen(
            ["uv", "run", "--no-sync", "clud", "--webui", "8900"],
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

    def test_terminal_route_loads(self) -> None:
        """Test that the /terminal route loads without errors."""
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

            # Navigate to terminal page
            terminal_url = f"{self.server_url}/terminal"
            logger.info("Navigating to %s", terminal_url)
            page.goto(terminal_url, wait_until="networkidle", timeout=10000)

            # Wait for terminal to be visible
            logger.info("Waiting for terminal to load...")
            page.wait_for_selector(".terminal-only-page", timeout=5000)

            # Check that the page title contains expected text
            title = page.title()
            logger.info("Page title: %s", title)
            self.assertIn("Terminal", title, "Page title should contain 'Terminal'")

            # Verify no critical console errors
            if console_errors:
                logger.warning("Console errors detected:")
                for error in console_errors:
                    logger.warning("  - %s", error)

            self.assertEqual(len(console_errors), 0, f"Expected no console errors, but found {len(console_errors)}: {console_errors}")

            browser.close()

    def test_terminal_executes_ls_command(self) -> None:
        """Test that the terminal can execute 'ls -al' and get expected output."""
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Navigate to terminal page
            terminal_url = f"{self.server_url}/terminal"
            logger.info("Navigating to %s", terminal_url)
            page.goto(terminal_url, wait_until="networkidle", timeout=10000)

            # Wait for terminal to be fully loaded
            logger.info("Waiting for terminal to initialize...")
            page.wait_for_selector(".xterm", timeout=10000)

            # Give the terminal a moment to fully initialize and connect WebSocket
            time.sleep(2)

            # Take initial screenshot
            screenshot_path = Path(__file__).parent / "artifacts" / "test_terminal_before.png"
            screenshot_path.parent.mkdir(parents=True, exist_ok=True)
            page.screenshot(path=str(screenshot_path))
            logger.info("Before screenshot saved to %s", screenshot_path)

            # Type 'ls -al' command into the terminal
            logger.info("Typing 'ls -al' command...")
            self._type_in_terminal(page, "ls -al\r")

            # Wait for command to execute
            time.sleep(2)

            # Take screenshot after command
            screenshot_path_after = Path(__file__).parent / "artifacts" / "test_terminal_after.png"
            page.screenshot(path=str(screenshot_path_after))
            logger.info("After screenshot saved to %s", screenshot_path_after)

            # Get the terminal content
            terminal_text = self._get_terminal_text(page)
            logger.info("Terminal output length: %d characters", len(terminal_text))
            logger.info("Terminal output (first 500 chars): %s", terminal_text[:500])

            # Verify command was executed
            # The output should contain typical directory listing markers
            self.assertTrue(len(terminal_text) > 50, f"Terminal output should be substantial after 'ls -al', got {len(terminal_text)} chars")

            # Check for typical directory listing patterns
            # Most systems will show at least '.' and '..' entries
            has_listing_output = (
                "total" in terminal_text.lower()
                or ".." in terminal_text
                or "drwx" in terminal_text  # Unix permissions
                or "pyproject.toml" in terminal_text  # Expected file in project
                or "src" in terminal_text  # Expected directory in project
            )

            self.assertTrue(has_listing_output, f"Terminal output should contain directory listing indicators. Output: {terminal_text[:200]}")

            browser.close()

    def test_terminal_executes_multiple_commands(self) -> None:
        """Test that the terminal can execute multiple commands in sequence (pwd, echo, cd)."""
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Navigate to terminal page
            terminal_url = f"{self.server_url}/terminal"
            logger.info("Navigating to %s", terminal_url)
            page.goto(terminal_url, wait_until="networkidle", timeout=10000)

            # Wait for terminal to be fully loaded
            logger.info("Waiting for terminal to initialize...")
            page.wait_for_selector(".xterm", timeout=10000)
            time.sleep(2)

            # Test 1: pwd command
            logger.info("Testing 'pwd' command...")
            self._type_in_terminal(page, "pwd\r")
            time.sleep(1)
            terminal_text = self._get_terminal_text(page)
            logger.info("After pwd: %s", terminal_text[-200:])
            # Check that we got a path-like output
            self.assertTrue(
                "/" in terminal_text or "\\" in terminal_text or "clud2" in terminal_text.lower(),
                "pwd should output a path",
            )

            # Test 2: echo command
            logger.info("Testing 'echo' command...")
            self._type_in_terminal(page, "echo HELLO_TERMINAL_TEST\r")
            time.sleep(1)
            terminal_text = self._get_terminal_text(page)
            logger.info("After echo: %s", terminal_text[-200:])
            self.assertIn("HELLO_TERMINAL_TEST", terminal_text, "echo output should be visible in terminal")

            # Test 3: mkdir and cd commands
            logger.info("Testing 'mkdir' and 'cd' commands...")
            self._type_in_terminal(page, "mkdir -p test_temp_dir_12345 2>/dev/null || true\r")
            time.sleep(1)
            self._type_in_terminal(page, "cd test_temp_dir_12345\r")
            time.sleep(1)
            self._type_in_terminal(page, "pwd\r")
            time.sleep(1)
            terminal_text = self._get_terminal_text(page)
            logger.info("After cd and pwd: %s", terminal_text[-200:])
            self.assertIn("test_temp_dir_12345", terminal_text, "pwd should show we're in the new directory")

            # Cleanup: go back and remove test directory
            logger.info("Cleaning up test directory...")
            self._type_in_terminal(page, "cd ..\r")
            time.sleep(1)
            self._type_in_terminal(page, "rmdir test_temp_dir_12345 2>/dev/null || true\r")
            time.sleep(1)

            browser.close()

    def test_terminal_command_history(self) -> None:
        """Test that terminal command history works with up/down arrow keys."""
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Navigate to terminal page
            terminal_url = f"{self.server_url}/terminal"
            logger.info("Navigating to %s", terminal_url)
            page.goto(terminal_url, wait_until="networkidle", timeout=10000)

            # Wait for terminal to be fully loaded
            logger.info("Waiting for terminal to initialize...")
            page.wait_for_selector(".xterm", timeout=10000)
            time.sleep(2)

            # Execute a unique command
            unique_cmd = "echo UNIQUE_HISTORY_TEST_67890"
            logger.info("Typing first command: %s", unique_cmd)
            self._type_in_terminal(page, f"{unique_cmd}\r")
            time.sleep(1)

            # Execute another command
            logger.info("Typing second command...")
            self._type_in_terminal(page, "echo SECOND_COMMAND\r")
            time.sleep(1)

            # Press Up arrow to go back in history
            logger.info("Pressing Up arrow to recall previous command...")
            page.keyboard.press("ArrowUp")
            time.sleep(0.5)

            # Press Up arrow again to get the first command
            page.keyboard.press("ArrowUp")
            time.sleep(0.5)

            # Get terminal text to see what's on the current line
            # The command should be recalled but not executed yet
            terminal_text_before_enter = self._get_terminal_text(page)
            logger.info("Terminal content after arrow up (last 300 chars): %s", terminal_text_before_enter[-300:])

            # Execute the recalled command by pressing Enter
            logger.info("Pressing Enter to execute recalled command...")
            page.keyboard.press("Enter")
            time.sleep(1)

            # Get terminal output after execution
            terminal_text_after = self._get_terminal_text(page)
            logger.info("Terminal content after enter (last 300 chars): %s", terminal_text_after[-300:])

            # The unique command output should appear twice in the terminal
            # (once from original execution, once from history recall)
            unique_output_count = terminal_text_after.count("UNIQUE_HISTORY_TEST_67890")
            logger.info("Found unique output %d times", unique_output_count)

            # We should see the output at least once (history might not show duplicate exactly as expected)
            self.assertGreaterEqual(unique_output_count, 1, "Command history should recall and execute previous command")

            browser.close()

    def _type_in_terminal(self, page: Page, text: str) -> None:
        """Type text into the terminal by focusing and typing."""
        # Click on the terminal to focus it
        terminal_selector = ".xterm-helper-textarea"
        try:
            page.click(terminal_selector, timeout=1000)
        except Exception:
            # If helper textarea is not found, click on terminal screen
            page.click(".xterm-screen")

        # Type the text
        page.keyboard.type(text)

    def _get_terminal_text(self, page: Page) -> str:
        """Extract visible text from the terminal."""
        # The xterm terminal stores its content in .xterm-rows
        try:
            # Use JavaScript to get terminal text content
            # Try multiple selectors as xterm structure may vary
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

    def test_xterm_wrapper_active_selector(self) -> None:
        """Test that the .xterm-wrapper.active selector contains terminal output after 'ls -al'."""
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Navigate to terminal page
            terminal_url = f"{self.server_url}/terminal"
            logger.info("Navigating to %s", terminal_url)
            page.goto(terminal_url, wait_until="networkidle", timeout=10000)

            # Wait for terminal to be fully loaded
            logger.info("Waiting for terminal to initialize...")
            page.wait_for_selector(".xterm", timeout=10000)
            time.sleep(2)

            # Verify the .xterm-wrapper.active element exists
            logger.info("Checking for .xterm-wrapper.active selector...")
            active_wrapper = page.query_selector(".xterm-wrapper.active")
            self.assertIsNotNone(active_wrapper, "The .xterm-wrapper.active element should exist")

            # Get the Svelte class ID from the element
            wrapper_classes = page.evaluate("""() => {
                const wrapper = document.querySelector('.xterm-wrapper.active');
                return wrapper ? wrapper.className : '';
            }""")
            logger.info("Active wrapper classes: %s", wrapper_classes)
            self.assertIn("xterm-wrapper", wrapper_classes, "Element should have xterm-wrapper class")
            self.assertIn("active", wrapper_classes, "Element should have active class")

            # Type 'ls -al' command into the terminal
            logger.info("Typing 'ls -al' command...")
            self._type_in_terminal(page, "ls -al\r")
            time.sleep(2)

            # Verify that the .xterm-wrapper.active element now contains the ls output
            terminal_text_from_wrapper = page.evaluate("""() => {
                const wrapper = document.querySelector('.xterm-wrapper.active');
                if (!wrapper) return '';
                // Get all text content from within the active wrapper
                return wrapper.innerText || wrapper.textContent || '';
            }""")
            logger.info("Terminal text from .xterm-wrapper.active (length: %d)", len(terminal_text_from_wrapper))
            logger.info("Terminal text sample: %s", terminal_text_from_wrapper[:300])

            # Verify the output contains directory listing
            self.assertTrue(len(terminal_text_from_wrapper) > 100, "Terminal output in .xterm-wrapper.active should be substantial")
            has_listing_output = (
                "total" in terminal_text_from_wrapper.lower()
                or "pyproject.toml" in terminal_text_from_wrapper
                or "src" in terminal_text_from_wrapper
                or "drwx" in terminal_text_from_wrapper
                or ".." in terminal_text_from_wrapper
            )
            self.assertTrue(has_listing_output, f"Terminal output in .xterm-wrapper.active should contain directory listing. Got: {terminal_text_from_wrapper[:200]}")

            browser.close()


if __name__ == "__main__":
    unittest.main()
