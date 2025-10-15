# pyright: reportUnknownVariableType=false, reportUnknownMemberType=false, reportUnknownArgumentType=false, reportUnknownParameterType=false, reportMissingImports=false
"""End-to-end tests for Diff tab in the Web UI using Playwright.

This test verifies that the Diff tab correctly displays file modifications
after making changes to files in the project directory.
Run with: bash test --full
"""

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


class TestDiffTabE2E(unittest.TestCase):
    """End-to-end tests for Diff tab functionality."""

    server_process: subprocess.Popen[bytes] | None = None
    server_url: str = "http://localhost:8902"
    startup_timeout: int = 30  # seconds
    test_project_dir: Path

    @classmethod
    def setUpClass(cls) -> None:
        """Start the Web UI server and set up test project directory."""
        logger.info("Setting up test environment for Diff tab e2e tests...")

        # Create a temporary test project directory
        cls.test_project_dir = Path(__file__).parent / "artifacts" / "test_diff_project"
        cls.test_project_dir.mkdir(parents=True, exist_ok=True)

        # Initialize git repo if not already initialized
        if not (cls.test_project_dir / ".git").exists():
            subprocess.run(
                ["git", "init"],
                cwd=str(cls.test_project_dir),
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["git", "config", "user.email", "test@example.com"],
                cwd=str(cls.test_project_dir),
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "Test User"],
                cwd=str(cls.test_project_dir),
                check=True,
                capture_output=True,
            )

            # Create initial README.md
            readme_path = cls.test_project_dir / "README.md"
            readme_path.write_text("# Test Project\n\nThis is a test project.\n", encoding="utf-8")

            # Commit the initial file
            subprocess.run(
                ["git", "add", "README.md"],
                cwd=str(cls.test_project_dir),
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["git", "commit", "-m", "Initial commit"],
                cwd=str(cls.test_project_dir),
                check=True,
                capture_output=True,
            )
        else:
            # If git repo already exists, reset to a clean state
            logger.info("Git repo already exists, resetting to clean state...")
            # Reset any changes
            subprocess.run(
                ["git", "reset", "--hard", "HEAD"],
                cwd=str(cls.test_project_dir),
                capture_output=True,
            )
            # Clean untracked files
            subprocess.run(
                ["git", "clean", "-fd"],
                cwd=str(cls.test_project_dir),
                capture_output=True,
            )

        logger.info("Test project directory set up at %s", cls.test_project_dir)

        # Start the Web UI server
        logger.info("Starting Web UI server for Diff tab e2e tests...")
        env = os.environ.copy()
        env["CLUD_NO_BROWSER"] = "1"

        cls.server_process = subprocess.Popen(
            ["uv", "run", "--no-sync", "clud", "--webui", "8902"],
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

    def test_diff_tab_shows_modified_file(self) -> None:
        """Test that the Diff tab correctly shows modifications to README.md."""
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

            # Intercept the /api/cwd request and return our test project path
            # This makes the app think the server's CWD is our test directory
            test_project_path = str(self.test_project_dir.resolve())
            logger.info("Will mock /api/cwd to return: %s", test_project_path)

            def handle_route(route: object) -> None:  # type: ignore[reportUnknownParameterType]
                """Mock route handler for /api/cwd endpoint."""
                if hasattr(route, "request") and route.request.url.endswith("/api/cwd"):  # type: ignore[reportUnknownMemberType,reportUnknownArgumentType]
                    route.fulfill(json={"cwd": test_project_path})  # type: ignore[reportUnknownMemberType]
                else:
                    route.continue_()  # type: ignore[reportUnknownMemberType]

            page.route("**/api/cwd", handle_route)

            # Navigate to Web UI
            logger.info("Navigating to %s", self.server_url)
            page.goto(self.server_url, wait_until="networkidle", timeout=10000)

            # Wait for app to load
            logger.info("Waiting for app to load...")
            page.wait_for_selector(".app-container", timeout=5000)

            # Wait for app to finish initializing (fetches /api/cwd and sets store)
            page.wait_for_timeout(1000)
            logger.info("App loaded and project path should be set")

            # Take initial screenshot
            screenshot_path = Path(__file__).parent / "artifacts" / "test_diff_tab_before.png"
            screenshot_path.parent.mkdir(parents=True, exist_ok=True)
            page.screenshot(path=str(screenshot_path))
            logger.info("Before screenshot saved to %s", screenshot_path)

            # Modify README.md by adding a new line
            logger.info("Modifying README.md...")
            readme_path = self.test_project_dir / "README.md"
            original_content = readme_path.read_text(encoding="utf-8")
            modified_content = original_content + "\nThis is a new line added for testing.\n"
            readme_path.write_text(modified_content, encoding="utf-8")
            logger.info("README.md modified successfully")

            # Trigger a diff scan through the API
            logger.info("Triggering diff scan...")
            import httpx

            # Use resolve() to get absolute normalized path
            normalized_path = str(self.test_project_dir.resolve())
            logger.info("Normalized project path: %s", normalized_path)
            response = httpx.post(
                f"{self.server_url}/api/diff/scan",
                json={"project_path": normalized_path},
                timeout=10.0,
            )
            logger.info("Diff scan response status: %d", response.status_code)
            scan_data = response.json()
            logger.info("Diff scan response: %s", scan_data)
            self.assertEqual(response.status_code, 200, "Diff scan should succeed")
            if "count" in scan_data:
                logger.info("Files found by scan: %d", scan_data["count"])

            # Wait a moment for the scan to complete
            time.sleep(1)

            # Click on Diff tab
            logger.info("Clicking Diff tab...")
            diff_tab_button = page.query_selector(".tab-button:has-text('Diff')")
            self.assertIsNotNone(diff_tab_button, "Diff tab button should exist")
            if diff_tab_button:
                diff_tab_button.click()

            # Wait for diff viewer to load
            time.sleep(2)

            # Verify Diff tab is now active
            active_tab = page.query_selector(".tab-button.active")
            self.assertIsNotNone(active_tab, "Should have an active tab after clicking")
            active_tab_text = active_tab.text_content() if active_tab else ""
            logger.info("Active tab after click: %s", active_tab_text)
            self.assertEqual(active_tab_text.strip(), "Diff", "Diff tab should be active")

            # Take screenshot after switching to Diff tab
            screenshot_path_after = Path(__file__).parent / "artifacts" / "test_diff_tab_after_switch.png"
            page.screenshot(path=str(screenshot_path_after))
            logger.info("After switch screenshot saved to %s", screenshot_path_after)

            # Check that README.md appears in the modified files list
            logger.info("Checking for README.md in modified files list...")
            page.wait_for_selector(".diff-navigator", timeout=5000)

            # Debug: Check what's in localStorage
            current_project_value = page.evaluate("localStorage.getItem('currentProject')")
            logger.info("Current project in localStorage: %s", current_project_value)

            # Also manually check the API to see if files are there
            logger.info("Checking /api/diff/tree directly...")
            test_proj_str = str(self.test_project_dir)
            logger.info("Test project dir string: %s", test_proj_str)
            tree_response = httpx.get(
                f"{self.server_url}/api/diff/tree",
                params={"path": test_proj_str},
                timeout=10.0,
            )
            logger.info("Tree API response status: %d", tree_response.status_code)
            tree_data = tree_response.json()
            logger.info("Tree API response: %s", tree_data)

            # Click the refresh button to load modified files
            logger.info("Clicking refresh button to load diffs...")
            refresh_button = page.query_selector(".diff-navigator .action-btn[title='Refresh']")
            if refresh_button:
                refresh_button.click()
                logger.info("Refresh button clicked")
            else:
                logger.warning("Refresh button not found!")
            time.sleep(2)

            # Wait for modified files to load
            time.sleep(1)

            # Take another screenshot after refresh
            screenshot_path_refreshed = Path(__file__).parent / "artifacts" / "test_diff_tab_after_refresh.png"
            page.screenshot(path=str(screenshot_path_refreshed))
            logger.info("After refresh screenshot saved to %s", screenshot_path_refreshed)

            # Debug: Check if there are any files listed
            all_elements = page.query_selector_all(".diff-tree-file, .diff-navigator-empty")
            logger.info("Elements found after refresh: %d", len(all_elements))
            for elem in all_elements:
                logger.info("Element text: %s", elem.text_content())

            # Look for README.md in the file tree
            readme_file_element = page.query_selector(".diff-tree-file:has-text('README.md')")
            self.assertIsNotNone(readme_file_element, "README.md should appear in modified files list")
            logger.info("README.md found in modified files list")

            # Click on README.md to view its diff
            logger.info("Clicking on README.md to view diff...")
            if readme_file_element:
                readme_file_element.click()

            # Wait for diff to render
            time.sleep(2)

            # Take screenshot showing the diff
            screenshot_path_diff = Path(__file__).parent / "artifacts" / "test_diff_tab_with_diff.png"
            page.screenshot(path=str(screenshot_path_diff))
            logger.info("Diff screenshot saved to %s", screenshot_path_diff)

            # Verify that the diff renderer shows content
            diff_renderer = page.query_selector(".diff-renderer-content")
            self.assertIsNotNone(diff_renderer, "Diff renderer should be present")

            # Try to wait for diff2html to render, but don't fail if it doesn't
            # The diff may render as plain text instead
            try:
                page.wait_for_selector(".d2h-file-wrapper", timeout=3000)
                logger.info("Diff has been rendered with diff2html")
            except Exception as e:
                logger.warning("diff2html didn't render (may use plain text fallback): %s", e)

            # Get the diff content
            diff_content = page.text_content(".diff-renderer-content")
            logger.info("Diff content length: %d characters", len(diff_content) if diff_content else 0)

            # Verify the new line appears in the diff
            self.assertIsNotNone(diff_content, "Diff content should not be None")
            if diff_content:
                self.assertIn("new line added for testing", diff_content, "Diff should show the new line")
                logger.info("Verified that diff shows the new line")

            # Take final screenshot
            screenshot_path_final = Path(__file__).parent / "artifacts" / "test_diff_tab_final.png"
            page.screenshot(path=str(screenshot_path_final))
            logger.info("Final screenshot saved to %s", screenshot_path_final)

            # Verify no critical console errors
            if console_errors:
                logger.warning("Console errors detected:")
                for error in console_errors:
                    logger.warning("  - %s", error)

            self.assertEqual(len(console_errors), 0, f"Expected no console errors, but found {len(console_errors)}: {console_errors}")

            browser.close()

    def test_diff_tab_multiple_file_changes(self) -> None:
        """Test that the Diff tab shows multiple modified files correctly."""
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Mock /api/cwd to return test project path
            test_project_path = str(self.test_project_dir.resolve())

            def handle_route(route: object) -> None:  # type: ignore[reportUnknownParameterType]
                """Mock route handler for /api/cwd endpoint."""
                if hasattr(route, "request") and route.request.url.endswith("/api/cwd"):  # type: ignore[reportUnknownMemberType,reportUnknownArgumentType]
                    route.fulfill(json={"cwd": test_project_path})  # type: ignore[reportUnknownMemberType]
                else:
                    route.continue_()  # type: ignore[reportUnknownMemberType]

            page.route("**/api/cwd", handle_route)

            # Navigate to Web UI
            logger.info("Navigating to %s", self.server_url)
            page.goto(self.server_url, wait_until="networkidle", timeout=10000)

            # Wait for app to load and initialize
            page.wait_for_selector(".app-container", timeout=5000)
            page.wait_for_timeout(1000)

            # Modify README.md
            readme_path = self.test_project_dir / "README.md"
            original_content = readme_path.read_text(encoding="utf-8")
            readme_path.write_text(original_content + "\nAnother modification.\n", encoding="utf-8")

            # Create a new file
            new_file_path = self.test_project_dir / "test_file.txt"
            new_file_path.write_text("This is a new test file.\n", encoding="utf-8")

            # Trigger diff scan
            logger.info("Triggering diff scan for multiple files...")
            import httpx

            response = httpx.post(
                f"{self.server_url}/api/diff/scan",
                json={"project_path": str(self.test_project_dir)},
                timeout=10.0,
            )
            self.assertEqual(response.status_code, 200, "Diff scan should succeed")
            time.sleep(1)

            # Switch to Diff tab
            logger.info("Switching to Diff tab...")
            diff_tab = page.query_selector(".tab-button:has-text('Diff')")
            if diff_tab:
                diff_tab.click()
            time.sleep(2)

            # Wait for modified files list to load
            page.wait_for_selector(".diff-navigator", timeout=5000)

            # Click the refresh button to load modified files
            logger.info("Clicking refresh button to load diffs...")
            refresh_button = page.query_selector(".diff-navigator .action-btn[title='Refresh']")
            if refresh_button:
                refresh_button.click()
            time.sleep(2)

            # Check that both files appear in the list
            modified_files = page.query_selector_all(".diff-tree-file")
            logger.info("Number of modified files: %d", len(modified_files))
            self.assertGreaterEqual(len(modified_files), 2, "Should show at least 2 modified files")

            # Take screenshot showing multiple files
            screenshot_path = Path(__file__).parent / "artifacts" / "test_diff_tab_multiple_files.png"
            page.screenshot(path=str(screenshot_path))
            logger.info("Multiple files screenshot saved to %s", screenshot_path)

            browser.close()


if __name__ == "__main__":
    unittest.main()
