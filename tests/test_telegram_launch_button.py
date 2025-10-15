"""End-to-end test for Telegram launch button.

This test reproduces the issue where the Telegram launch button remains
disabled even when valid credentials are saved.

Run with: bash test --full
"""

import logging
import os
import subprocess
import time
import unittest
from pathlib import Path

from playwright.sync_api import sync_playwright

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class TestTelegramLaunchButton(unittest.TestCase):
    """E2E test for Telegram launch button functionality."""

    server_process: subprocess.Popen[bytes] | None = None
    server_url: str = "http://localhost:8898"
    startup_timeout: int = 30  # seconds

    @classmethod
    def setUpClass(cls) -> None:
        """Start the Web UI server before running tests."""
        logger.info("Starting Web UI server for Telegram button test...")

        # Start the server on port 8898 to avoid conflicts
        env = os.environ.copy()
        env["CLUD_NO_BROWSER"] = "1"

        cls.server_process = subprocess.Popen(
            ["uv", "run", "--no-sync", "clud", "--webui", "8898"],
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

    def test_telegram_button_enabled_with_valid_credentials(self) -> None:
        """Test that Telegram launch button is enabled when valid credentials exist.

        This test reproduces the bug where the button remains disabled even
        when a valid bot token is saved but the API test fails (e.g., network issue).
        It simulates having cached bot_info from a previous successful connection.
        """
        import httpx

        # First, save Telegram credentials
        # Using a fake token format that matches Telegram's bot token pattern
        # This token will fail the API test, but we'll simulate cached bot_info
        test_bot_token = "1234567890:ABCdefGHIjklMNOpqrsTUVwxyz123456789"

        logger.info("Saving test Telegram credentials...")
        response = httpx.post(
            f"{self.server_url}/api/telegram/credentials",
            json={"bot_token": test_bot_token, "chat_id": None},
            timeout=5.0,
        )
        logger.info("Credentials save response: %s", response.json())

        # Now check the status (will fail API test since token is fake)
        logger.info("Checking Telegram status...")
        status_response = httpx.get(f"{self.server_url}/api/telegram/status", timeout=5.0)
        status_data = status_response.json()
        logger.info("Telegram status: %s", status_data)

        # Launch browser and check button state
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Navigate to Web UI
            logger.info("Navigating to %s", self.server_url)
            page.goto(self.server_url, wait_until="networkidle", timeout=10000)

            # Wait for page to load
            page.wait_for_selector("body", timeout=5000)

            # Inject cached bot_info into localStorage to simulate previous successful connection
            # This is the key: even though the API test failed, we have cached bot info
            logger.info("Injecting cached bot_info into localStorage...")
            page.evaluate(
                """
                localStorage.setItem('telegram_bot_info', JSON.stringify({
                    id: 1234567890,
                    username: 'test_bot',
                    first_name: 'Test Bot',
                    deep_link: 'https://t.me/test_bot'
                }));
            """
            )

            # Reload page to apply cached bot_info
            logger.info("Reloading page to apply cached bot_info...")
            page.reload(wait_until="networkidle", timeout=10000)

            # Wait a bit for the status fetch to complete
            time.sleep(2)

            # Check if Telegram button exists
            telegram_button = page.query_selector('[data-testid="telegram-launch-button"]')

            if telegram_button is None:
                logger.warning("Telegram button not found - credentials may not be showing UI")
                # Take screenshot for debugging
                screenshot_path = Path(__file__).parent.parent / "test_telegram_button_not_found.png"
                page.screenshot(path=str(screenshot_path))
                logger.info("Screenshot saved to %s", screenshot_path)
            else:
                # Check if button is disabled
                is_disabled = telegram_button.get_attribute("disabled")
                button_html = telegram_button.evaluate("el => el.outerHTML")
                logger.info("Telegram button HTML: %s", button_html)
                logger.info("Button disabled attribute: %s", is_disabled)

                # Take screenshot
                screenshot_path = Path(__file__).parent.parent / "test_telegram_button_state.png"
                page.screenshot(path=str(screenshot_path))
                logger.info("Screenshot saved to %s", screenshot_path)

                # The button should NOT be disabled when credentials are saved
                # This assertion will fail with the current bug
                self.assertIsNone(
                    is_disabled,
                    f"Telegram launch button should be enabled when valid credentials are saved, but it has disabled={is_disabled}. Status: {status_data}",
                )

            browser.close()

        # Cleanup: remove credentials
        logger.info("Cleaning up test credentials...")
        try:
            httpx.delete(f"{self.server_url}/api/telegram/credentials", timeout=10.0)
        except httpx.ReadTimeout:
            logger.warning("Cleanup timed out - server may have issues")


if __name__ == "__main__":
    unittest.main()
