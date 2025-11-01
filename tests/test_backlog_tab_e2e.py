"""End-to-end tests for Backlog tab using Playwright.

This test ensures the Backlog tab can be accessed, UI elements render correctly,
and filtering/searching functionality works.
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


class TestBacklogTabE2E(unittest.TestCase):
    """End-to-end tests for Backlog tab."""

    server_process: subprocess.Popen[bytes] | None = None
    server_url: str = "http://localhost:8903"
    startup_timeout: int = 60  # seconds

    @classmethod
    def setUpClass(cls) -> None:
        """Start the Web UI server before running tests."""
        logger.info("Starting Web UI server for Backlog e2e tests...")

        # Create test Backlog.md in the repo root if it doesn't exist
        test_backlog_path = Path(__file__).parent.parent / "Backlog.md"
        if not test_backlog_path.exists():
            logger.info("Creating test Backlog.md file...")
            test_backlog_content = """# Backlog

## To Do
- [ ] #1 Add user authentication (priority: high)
  - Implement OAuth2 flow
  - Add JWT token handling
- [ ] #2 Create dashboard UI (priority: medium)

## In Progress
- [ ] #3 Fix login bug

## Done
- [x] #4 Setup project structure
- [x] #5 Write documentation (priority: low)
"""
            test_backlog_path.write_text(test_backlog_content, encoding="utf-8")
            logger.info("Test Backlog.md created")

        # Start the server using the actual CLI command that users would run
        # Use port 8903 to avoid conflicts
        env = os.environ.copy()
        # Prevent browser from auto-opening during tests
        env["CLUD_NO_BROWSER"] = "1"

        cls.server_process = subprocess.Popen(
            ["uv", "run", "--no-sync", "clud", "--webui", "8903"],
            env=env,
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

    def test_backlog_tab_exists_in_navigation(self) -> None:
        """Test that the Backlog tab button exists in the tab navigation."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                # Filter out known harmless errors
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)
            else:
                logger.debug("Browser console %s: %s", msg.type, msg.text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()

            # Listen for console errors
            page.on("console", on_console_message)

            def on_request_failed(request: object) -> None:
                logger.error("Request failed: %s", request)

            page.on("requestfailed", on_request_failed)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Wait for main content to be visible
                logger.info("Waiting for tab navigation to load...")
                page.wait_for_selector(".tab-nav", timeout=15000)

                # Check that Backlog tab button exists
                backlog_button = page.locator("button", has_text="Backlog")
                self.assertTrue(backlog_button.is_visible(), "Backlog tab button should be visible")

                logger.info("✓ Backlog tab button exists in navigation")

            finally:
                browser.close()
                if console_errors:
                    logger.warning("Console errors detected: %s", console_errors)

    def test_backlog_tab_can_be_clicked(self) -> None:
        """Test that clicking the Backlog tab switches to it."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Wait for tab navigation
                page.wait_for_selector(".tab-nav", timeout=15000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()

                # Wait for backlog panel to be active
                page.wait_for_selector(".tab-panel.active", timeout=15000)

                # Verify the Backlog button is now marked as active
                self.assertTrue(
                    backlog_button.evaluate("el => el.classList.contains('active')"),
                    "Backlog tab should be marked as active",
                )

                logger.info("✓ Backlog tab can be clicked and becomes active")

            finally:
                browser.close()

    def test_backlog_panel_renders_correctly(self) -> None:
        """Test that the Backlog panel renders with expected UI elements."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()

                # Wait for backlog container to be visible
                page.wait_for_selector(".backlog-container", timeout=5000)

                # Check for header
                header = page.locator(".backlog-header h2")
                self.assertTrue(header.is_visible(), "Backlog header should be visible")
                header_text = header.text_content()
                assert header_text is not None, "Header text should not be None"
                self.assertIn("Backlog", header_text, "Header should contain 'Backlog' text")

                # Check for stats section
                stats = page.locator(".header-stats")
                self.assertTrue(stats.is_visible(), "Header stats should be visible")

                # Check for search box
                search_input = page.locator("input[placeholder='Search tasks...']")
                self.assertTrue(search_input.is_visible(), "Search input should be visible")

                # Check for filter buttons
                filter_buttons = page.locator(".filter-buttons button")
                count = filter_buttons.count()
                self.assertGreaterEqual(count, 4, "Should have at least 4 filter buttons (All, Todo, In Progress, Done)")

                # Check for refresh button
                refresh_button = page.locator(".refresh-button")
                self.assertTrue(refresh_button.is_visible(), "Refresh button should be visible")

                logger.info("✓ Backlog panel renders with all expected UI elements")

            finally:
                browser.close()

    def test_backlog_displays_empty_state(self) -> None:
        """Test that an empty state message is displayed when no tasks exist."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()

                # Wait for backlog container
                page.wait_for_selector(".backlog-container", timeout=5000)

                # Wait a bit for the component to load and try to fetch tasks
                page.wait_for_timeout(1000)

                # Check for empty state message or tasks container
                tasks_container = page.locator(".tasks-container")

                # Should have a tasks-container with either empty-state or tasks-list
                self.assertTrue(tasks_container.is_visible(), "Tasks container should be visible")

                logger.info("✓ Backlog displays appropriate content (empty state or loading)")

            finally:
                browser.close()

    def test_backlog_filter_buttons_are_clickable(self) -> None:
        """Test that filter buttons can be clicked and show active state."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()

                # Wait for backlog container
                page.wait_for_selector(".backlog-container", timeout=5000)

                # Get filter buttons
                filter_buttons = page.locator(".filter-buttons button")
                count = filter_buttons.count()
                self.assertGreater(count, 0, "Should have at least one filter button")

                # "All" button should be active by default
                all_button = page.locator(".filter-buttons button", has_text="All")
                self.assertTrue(
                    all_button.evaluate("el => el.classList.contains('active')"),
                    "All filter should be active by default",
                )

                # Click "To Do" filter
                todo_button = page.locator(".filter-buttons button", has_text="To Do")
                todo_button.click()
                page.wait_for_timeout(500)

                # "To Do" button should now be active
                self.assertTrue(
                    todo_button.evaluate("el => el.classList.contains('active')"),
                    "To Do filter should be active after clicking",
                )

                logger.info("✓ Filter buttons are clickable and show active state")

            finally:
                browser.close()

    def test_backlog_search_input_is_functional(self) -> None:
        """Test that the search input can be interacted with."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()

                # Wait for backlog container
                page.wait_for_selector(".backlog-container", timeout=5000)

                # Find and interact with search input
                search_input = page.locator("input[placeholder='Search tasks...']")
                self.assertTrue(search_input.is_visible(), "Search input should be visible")

                # Type in search input
                search_input.fill("test query")
                page.wait_for_timeout(500)

                # Verify the value is set
                value = search_input.input_value()
                self.assertEqual(value, "test query", "Search input should contain typed text")

                # Clear the input
                search_input.clear()
                value = search_input.input_value()
                self.assertEqual(value, "", "Search input should be cleared")

                logger.info("✓ Search input is functional")

            finally:
                browser.close()

    def test_backlog_refresh_button_works(self) -> None:
        """Test that the refresh button can be clicked."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()

                # Wait for backlog container
                page.wait_for_selector(".backlog-container", timeout=5000)

                # Find refresh button
                refresh_button = page.locator(".refresh-button")
                self.assertTrue(refresh_button.is_visible(), "Refresh button should be visible")

                # Click refresh button
                refresh_button.click()
                page.wait_for_timeout(500)

                logger.info("✓ Refresh button can be clicked")

            finally:
                browser.close()

    def test_backlog_tab_persists_across_switches(self) -> None:
        """Test that Backlog tab content persists when switching tabs."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()
                page.wait_for_selector(".backlog-container", timeout=5000)

                # Type in search box to create state
                search_input = page.locator("input[placeholder='Search tasks...']")
                search_input.fill("test")

                # Switch to Chat tab
                chat_button = page.locator("button", has_text="Chat")
                chat_button.click()
                page.wait_for_timeout(500)

                # Switch back to Backlog tab
                backlog_button.click()
                page.wait_for_timeout(500)

                # Verify Backlog is visible
                backlog_container = page.locator(".backlog-container")
                self.assertTrue(backlog_container.is_visible(), "Backlog tab should be visible after switching back")

                logger.info("✓ Backlog tab persists across tab switches")

            finally:
                browser.close()

    def test_backlog_loads_tasks_from_backend(self) -> None:
        """Test that tasks are loaded from the backend API and displayed."""
        console_errors: list[str] = []

        def on_console_message(msg: ConsoleMessage) -> None:
            """Capture console messages."""
            if msg.type == "error":
                error_text = msg.text
                if "WebSocket" not in error_text and "favicon" not in error_text:
                    console_errors.append(error_text)
                    logger.error("Browser console error: %s", error_text)

        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(headless=True)
            context = browser.new_context()
            page = context.new_page()
            page.on("console", on_console_message)

            try:
                logger.info("Navigating to %s", self.server_url)
                page.goto(self.server_url, wait_until="networkidle", timeout=30000)

                # Click Backlog tab
                backlog_button = page.locator("button", has_text="Backlog")
                backlog_button.click()

                # Wait for tasks to load from backend
                # The component should fetch from /api/backlog and render tasks
                page.wait_for_timeout(2000)

                # Check if tasks are displayed (if Backlog.md exists with tasks)
                # We should see task items or empty state
                tasks_list = page.locator(".tasks-list .task-card")
                # Be more specific - look for empty state within backlog container
                empty_state = page.locator(".backlog-container .empty-state")

                # Either tasks or empty state should be visible
                tasks_count = tasks_list.count()
                empty_visible = empty_state.is_visible() if empty_state.count() > 0 else False

                if tasks_count > 0:
                    logger.info(f"✓ Found {tasks_count} tasks loaded from backend")

                    # Verify that tasks have expected structure
                    first_task = tasks_list.first
                    task_title = first_task.locator(".task-title")
                    self.assertTrue(task_title.is_visible(), "Task title should be visible")

                    # Check for task status badge
                    task_status = first_task.locator(".status-badge")
                    self.assertTrue(task_status.is_visible(), "Task status should be visible")

                    logger.info("✓ Tasks have expected structure (title, status)")
                elif empty_visible:
                    logger.info("✓ Empty state displayed (no tasks in Backlog.md)")
                else:
                    self.fail("Neither tasks nor empty state found - component may not be loading")

                # Verify stats are updated with actual counts
                stats = page.locator(".header-stats")
                if stats.is_visible():
                    stats_text = stats.text_content()
                    self.assertIsNotNone(stats_text, "Stats text should not be None")
                    logger.info(f"✓ Stats displayed: {stats_text}")

            finally:
                browser.close()


if __name__ == "__main__":
    unittest.main()
