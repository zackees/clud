"""End-to-end tests for Web UI using Playwright.

This test ensures the Web UI can load properly and displays without console errors.
Run with: bash test --full
"""

# Playwright has incomplete type stubs - disable type checking for third-party import errors
# pyright: reportMissingImports=false, reportUnknownVariableType=false, reportUnknownMemberType=false, reportUnknownParameterType=false, reportUnknownArgumentType=false

import logging
import os
import subprocess
import time
import unittest
from pathlib import Path

from playwright.sync_api import ConsoleMessage, sync_playwright

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class TestWebUIE2E(unittest.TestCase):
    """End-to-end tests for Web UI."""

    server_process: subprocess.Popen[bytes] | None = None
    server_url: str = "http://localhost:8899"
    startup_timeout: int = 30  # seconds

    @classmethod
    def setUpClass(cls) -> None:
        """Start the Web UI server before running tests."""
        logger.info("Starting Web UI server for e2e tests...")

        # Start the server using the actual CLI command that users would run
        # Use port 8899 to avoid conflicts with default 8888
        # This tests the full CLI entry point, not just the server module
        # Use "uv run --no-sync" to avoid reinstalling and ensure we test local source
        env = os.environ.copy()
        # Prevent browser from auto-opening during tests
        env["CLUD_NO_BROWSER"] = "1"

        cls.server_process = subprocess.Popen(
            ["uv", "run", "--no-sync", "clud", "--webui", "8899"],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
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

    def test_webui_loads_without_errors(self) -> None:
        """Test that the Web UI page loads and displays without console errors."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                # Filter out known harmless errors
                # e.g., WebSocket connection errors during shutdown are expected
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)
            else:
                # Log all console messages for debugging
                logger.debug("Browser console %s: %s", msg.type, msg.text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Listen for console errors
            page.on("console", on_console_message)

            # Log failed requests
            def on_request_failed(request: object) -> None:
                logger.error("Request failed: %s", request)

            page.on("requestfailed", on_request_failed)

            # Navigate to Web UI
            logger.info("Navigating to %s", self.server_url)
            page.goto(self.server_url, wait_until="networkidle", timeout=10000)

            # Wait for main content to be visible
            logger.info("Waiting for main content to load...")
            page.wait_for_selector("body", timeout=5000)

            # Check that the page title contains expected text
            title = page.title()
            logger.info("Page title: %s", title)
            self.assertIn("Claude", title, "Page title should contain 'Claude'")

            # Take a screenshot for debugging if needed
            screenshot_path = Path(__file__).parent.parent / "test_webui_screenshot.png"
            page.screenshot(path=str(screenshot_path))
            logger.info("Screenshot saved to %s", screenshot_path)

            # Verify no critical console errors
            if console_errors:
                logger.warning("Console errors detected:")
                for error in console_errors:
                    logger.warning("  - %s", error)

            self.assertEqual(len(console_errors), 0, f"Expected no console errors, but found {len(console_errors)}: {console_errors}")

            browser.close()

    def test_health_endpoint(self) -> None:
        """Test that the health endpoint returns OK."""
        import httpx

        response = httpx.get(f"{self.server_url}/health", timeout=5.0)
        self.assertEqual(response.status_code, 200)
        data = response.json()
        self.assertEqual(data["status"], "ok")

    def test_cwd_endpoint(self) -> None:
        """Test that the cwd endpoint returns current working directory."""
        import httpx

        response = httpx.get(f"{self.server_url}/api/cwd", timeout=5.0)
        self.assertEqual(response.status_code, 200)
        data = response.json()
        self.assertIn("cwd", data)
        self.assertTrue(os.path.isdir(data["cwd"]))


if __name__ == "__main__":
    unittest.main()
