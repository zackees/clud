"""Test Shift+Enter behavior in Web UI chat input."""

# Playwright has incomplete type stubs - disable type checking for third-party import errors
# pyright: reportMissingImports=false, reportUnknownVariableType=false, reportUnknownMemberType=false, reportUnknownParameterType=false, reportUnknownArgumentType=false

import asyncio
import contextlib
import logging
import os
import subprocess
import time
import unittest
from pathlib import Path

from playwright.async_api import async_playwright, expect

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class TestShiftEnter(unittest.TestCase):
    """Test that Shift+Enter creates newlines instead of submitting."""

    server_process: subprocess.Popen[bytes] | None = None
    server_url: str = "http://localhost:8904"
    startup_timeout: int = 60  # seconds

    @classmethod
    def setUpClass(cls) -> None:
        """Start the Web UI server before running tests."""
        logger.info("Starting Web UI server for Shift+Enter tests...")

        # Start the server using the actual CLI command that users would run
        # Use port 8904 to avoid conflicts
        env = os.environ.copy()
        # Prevent browser from auto-opening during tests
        env["CLUD_NO_BROWSER"] = "1"

        cls.server_process = subprocess.Popen(
            ["uv", "run", "--no-sync", "clud", "--webui", "8904"],
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

    def test_shift_enter_creates_newline(self) -> None:
        """Test that Shift+Enter in chat input creates newline without submitting."""
        asyncio.run(self._test_shift_enter_async())

    async def _test_shift_enter_async(self) -> None:
        """Async test implementation."""
        async with async_playwright() as p:
            browser = await p.chromium.launch(headless=True)
            page = await browser.new_page()

            try:
                # Navigate to web UI
                await page.goto(self.server_url, timeout=10000)

                # Wait for page to load and get the chat input textarea specifically
                await page.wait_for_selector("textarea.message-input", timeout=5000)

                # Get the chat textarea element (not the terminal textarea)
                textarea = page.locator("textarea.message-input")
                await expect(textarea).to_be_visible()

                # Focus the textarea
                await textarea.focus()

                # Type first line
                await textarea.fill("Line 1")

                # Count messages before Shift+Enter
                messages_before = await page.locator(".message.user").count()

                # Press Shift+Enter
                await page.keyboard.press("Shift+Enter")

                # Wait a bit to ensure no message sent
                await page.wait_for_timeout(500)

                # Count messages after Shift+Enter
                messages_after = await page.locator(".message.user").count()

                # Verify no new message was sent
                self.assertEqual(messages_before, messages_after, "Shift+Enter should NOT submit message")

                # Type second line
                await page.keyboard.type("Line 2")

                # Get textarea content
                textarea_value = await textarea.input_value()

                # Verify textarea contains newline
                self.assertIn("\n", textarea_value, "Textarea should contain newline character")
                self.assertIn("Line 1", textarea_value, "Textarea should contain first line")
                self.assertIn("Line 2", textarea_value, "Textarea should contain second line")

                # Verify the textarea shows multiline content
                expected_content = "Line 1\nLine 2"
                self.assertEqual(textarea_value, expected_content, f"Textarea content should be '{expected_content}' but got '{textarea_value}'")

            finally:
                await browser.close()

    def test_enter_submits_message(self) -> None:
        """Test that Enter (without Shift) submits the message."""
        asyncio.run(self._test_enter_submits_async())

    async def _test_enter_submits_async(self) -> None:
        """Async test implementation for Enter key."""
        async with async_playwright() as p:
            browser = await p.chromium.launch(headless=True)
            page = await browser.new_page()

            try:
                # Navigate to web UI
                await page.goto(self.server_url, timeout=10000)

                # Wait for page to load and get the chat input textarea specifically
                await page.wait_for_selector("textarea.message-input", timeout=5000)

                # Get the chat textarea element (not the terminal textarea)
                textarea = page.locator("textarea.message-input")
                await expect(textarea).to_be_visible()

                # Focus the textarea
                await textarea.focus()

                # Type a message
                test_message = "Test message for Enter key"
                await textarea.fill(test_message)

                # Count messages before Enter
                messages_before = await page.locator(".message.user").count()

                # Press Enter (without Shift)
                await page.keyboard.press("Enter")

                # Wait for message to appear (with timeout)
                # Message might already exist from previous tests
                with contextlib.suppress(Exception):
                    await page.wait_for_selector(".message.user", timeout=3000)

                # Wait a bit for processing
                await page.wait_for_timeout(500)

                # Count messages after Enter
                messages_after = await page.locator(".message.user").count()

                # Verify message was sent (should have one more message)
                self.assertGreater(messages_after, messages_before, "Enter key should submit message")

                # Verify textarea was cleared after send
                textarea_value = await textarea.input_value()
                self.assertEqual("", textarea_value, "Textarea should be empty after sending with Enter")

            finally:
                await browser.close()


if __name__ == "__main__":
    unittest.main()
