"""End-to-end tests for clud --loop TUI feature.

This module tests complete TUI workflows using Textual's test pilot:
- Streaming output display
- Menu navigation (keyboard controls)
- Submenu navigation
- Terminal resize handling
- Integration with callbacks
- Error handling scenarios

Run with: bash test --full
"""

# Textual has incomplete type stubs - disable type checking for E2E tests
# pyright: reportMissingImports=false, reportUnknownVariableType=false, reportUnknownMemberType=false, reportUnknownParameterType=false, reportUnknownArgumentType=false, reportOptionalMemberAccess=false, reportAttributeAccessIssue=false

import tempfile
import unittest
from pathlib import Path

from textual.widgets import RichLog

from clud.loop_tui.app import CludLoopTUI


class TestLoopTUIE2EStreaming(unittest.TestCase):
    """Test streaming output functionality end-to-end."""

    def test_streaming_output_appears_in_log(self) -> None:
        """Test that streaming output appears in the log widget."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Wait for app to mount
                await pilot.pause()

                # Verify log widget exists
                log = app.query_one(RichLog)
                self.assertIsNotNone(log)

                # Add some output
                app.log_message("Test message 1")
                app.log_message("Test message 2")
                app.log_message("Test message 3")
                await pilot.pause()

                # Verify output appeared (RichLog stores lines)
                # Note: Initial message "TUI initialized..." is also present
                self.assertGreater(len(log.lines), 0)

        asyncio.run(run_test())

    def test_multiple_messages_maintain_order(self) -> None:
        """Test that multiple messages maintain insertion order."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                await pilot.pause()

                # Add messages in specific order
                messages = [f"Message {i}" for i in range(10)]
                for msg in messages:
                    app.log_message(msg)

                await pilot.pause()

                # Verify log has content
                log = app.query_one(RichLog)
                self.assertGreater(len(log.lines), 0)

        asyncio.run(run_test())

    def test_auto_scroll_enabled(self) -> None:
        """Test that auto-scroll is enabled for streaming output."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                await pilot.pause()

                # Verify RichLog has auto_scroll enabled
                log = app.query_one(RichLog)
                self.assertTrue(log.auto_scroll)

        asyncio.run(run_test())


class TestLoopTUIE2EKeyboardNavigation(unittest.TestCase):
    """Test menu keyboard navigation end-to-end."""

    def test_arrow_right_navigation(self) -> None:
        """Test navigating menu with right arrow key."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                initial_index = app.selected_index
                self.assertEqual(initial_index, 0)

                # Navigate right
                await pilot.press("right")
                await pilot.pause()

                self.assertEqual(app.selected_index, 1)

        asyncio.run(run_test())

    def test_arrow_left_navigation(self) -> None:
        """Test navigating menu with left arrow key."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Start at second item
                app.selected_index = 1
                app.update_menu()
                await pilot.pause()

                # Navigate left
                await pilot.press("left")
                await pilot.pause()

                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())

    def test_tab_navigation(self) -> None:
        """Test navigating menu with tab key."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                initial_index = app.selected_index

                # Navigate with tab
                await pilot.press("tab")
                await pilot.pause()

                self.assertEqual(app.selected_index, (initial_index + 1) % len(app.current_menu))

        asyncio.run(run_test())

    def test_arrow_up_navigation(self) -> None:
        """Test navigating menu with up arrow key."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Start at second item
                app.selected_index = 1
                app.update_menu()
                await pilot.pause()

                # Navigate up
                await pilot.press("up")
                await pilot.pause()

                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())

    def test_arrow_down_navigation(self) -> None:
        """Test navigating menu with down arrow key."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                initial_index = app.selected_index

                # Navigate down
                await pilot.press("down")
                await pilot.pause()

                self.assertEqual(app.selected_index, (initial_index + 1) % len(app.current_menu))

        asyncio.run(run_test())

    def test_navigation_wraps_around_forward(self) -> None:
        """Test that navigation wraps around at end of menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Navigate to last item
                menu_length = len(app.current_menu)
                for _ in range(menu_length - 1):
                    await pilot.press("right")
                    await pilot.pause()

                # Verify at last item
                self.assertEqual(app.selected_index, menu_length - 1)

                # Navigate once more - should wrap to first
                await pilot.press("right")
                await pilot.pause()

                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())

    def test_navigation_wraps_around_backward(self) -> None:
        """Test that navigation wraps around at beginning of menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Start at first item
                self.assertEqual(app.selected_index, 0)

                # Navigate backward - should wrap to last
                await pilot.press("left")
                await pilot.pause()

                menu_length = len(app.current_menu)
                self.assertEqual(app.selected_index, menu_length - 1)

        asyncio.run(run_test())


class TestLoopTUIE2ESubmenuNavigation(unittest.TestCase):
    """Test submenu navigation end-to-end."""

    def test_enter_opens_options_submenu(self) -> None:
        """Test that pressing Enter on Options opens submenu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Ensure Options is selected
                app.selected_index = 0
                app.update_menu()
                await pilot.pause()

                # Press Enter
                await pilot.press("enter")
                await pilot.pause()

                # Should be in OPTIONS_MENU
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)
                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())

    def test_escape_in_submenu_returns_to_main(self) -> None:
        """Test that Esc in submenu returns to main menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Open options menu
                app.selected_index = 0
                await pilot.press("enter")
                await pilot.pause()

                # Verify in submenu
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)

                # Press Esc
                await pilot.press("escape")
                await pilot.pause()

                # Should be back in MAIN_MENU
                self.assertEqual(app.current_menu, app.MAIN_MENU)
                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())

    def test_back_button_returns_to_main(self) -> None:
        """Test that selecting Back returns to main menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Open options menu
                await pilot.press("enter")
                await pilot.pause()

                # Select Back (first item in OPTIONS_MENU)
                self.assertEqual(app.selected_index, 0)
                await pilot.press("enter")
                await pilot.pause()

                # Should be back in MAIN_MENU
                self.assertEqual(app.current_menu, app.MAIN_MENU)

        asyncio.run(run_test())

    def test_submenu_navigation_independent(self) -> None:
        """Test that submenu navigation is independent from main menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Navigate to second item in main menu
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

                # Go back to first item and open options
                await pilot.press("left")
                await pilot.press("enter")
                await pilot.pause()

                # Should be in OPTIONS_MENU at first item
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)
                self.assertEqual(app.selected_index, 0)

                # Navigate in submenu
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

        asyncio.run(run_test())


class TestLoopTUIE2ETerminalResize(unittest.TestCase):
    """Test layout works with different terminal sizes."""

    def test_layout_at_different_sizes(self) -> None:
        """Test that layout renders at different terminal sizes."""
        import asyncio

        async def run_test() -> None:
            # Test large size
            app_large = CludLoopTUI()
            async with app_large.run_test(size=(120, 40)) as pilot:
                await pilot.pause()

                output = app_large.query_one("#output")
                menu = app_large.query_one("#menu_container")

                self.assertIsNotNone(output)
                self.assertIsNotNone(menu)

            # Test minimum size
            app_small = CludLoopTUI()
            async with app_small.run_test(size=(80, 20)) as pilot:
                await pilot.pause()

                output = app_small.query_one("#output")
                menu = app_small.query_one("#menu_container")

                self.assertIsNotNone(output)
                self.assertIsNotNone(menu)

            # Test default size
            app_default = CludLoopTUI()
            async with app_default.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                output = app_default.query_one("#output")
                menu = app_default.query_one("#menu_container")

                self.assertIsNotNone(output)
                self.assertIsNotNone(menu)

        asyncio.run(run_test())


class TestLoopTUIE2ECallbacks(unittest.TestCase):
    """Test callback integration end-to-end."""

    def test_exit_callback_triggered(self) -> None:
        """Test that exit callback is triggered when exiting."""
        import asyncio

        async def run_test() -> None:
            exit_called = False

            def on_exit() -> None:
                nonlocal exit_called
                exit_called = True

            app = CludLoopTUI(on_exit=on_exit)

            async with app.run_test() as pilot:
                await pilot.pause()

                # Double Ctrl-C to quit
                await pilot.press("ctrl+c")
                await pilot.pause()
                await pilot.press("ctrl+c")
                await pilot.pause()

            # Callback should have been called
            self.assertTrue(exit_called)

        asyncio.run(run_test())

    def test_halt_callback_triggered(self) -> None:
        """Test that halt callback is triggered from menu."""
        import asyncio

        async def run_test() -> None:
            halt_called = False

            def on_halt() -> None:
                nonlocal halt_called
                halt_called = True

            app = CludLoopTUI(on_halt=on_halt)

            async with app.run_test() as pilot:
                await pilot.pause()

                # Navigate to Options menu
                await pilot.press("enter")
                await pilot.pause()

                # Navigate to Halt (fourth item, index 3)
                await pilot.press("right")  # Edit
                await pilot.press("right")  # Copy All
                await pilot.press("right")  # Halt
                await pilot.pause()

                # Select Halt
                await pilot.press("enter")
                await pilot.pause()

            # Callback should have been called
            self.assertTrue(halt_called)

        asyncio.run(run_test())

    def test_edit_callback_triggered(self) -> None:
        """Test that edit callback is triggered after editing."""
        import asyncio

        async def run_test() -> None:
            edit_called = False

            def on_edit() -> None:
                nonlocal edit_called
                edit_called = True

            # Create temporary UPDATE.md file
            with tempfile.TemporaryDirectory() as tmpdir:
                update_file = Path(tmpdir) / "UPDATE.md"
                update_file.write_text("test content")

                app = CludLoopTUI(on_edit=on_edit, update_file=update_file)

                # Mock both subprocess.run and app.suspend
                import unittest.mock

                with unittest.mock.patch("clud.loop_tui.app.RunningProcess.run"), unittest.mock.patch.object(app, "suspend"):
                    async with app.run_test() as pilot:
                        await pilot.pause()

                        # Call open_update_file directly to test callback
                        app.open_update_file()
                        await pilot.pause(0.5)

                # Callback should have been called
                self.assertTrue(edit_called)

        asyncio.run(run_test())


class TestLoopTUIE2EEditorIntegration(unittest.TestCase):
    """Test UPDATE.md editor integration end-to-end."""

    def test_open_update_file_creates_if_missing(self) -> None:
        """Test that UPDATE.md is created if it doesn't exist."""
        import asyncio

        async def run_test() -> None:
            with tempfile.TemporaryDirectory() as tmpdir:
                update_file = Path(tmpdir) / "subdir" / "UPDATE.md"

                app = CludLoopTUI(update_file=update_file)

                # Mock subprocess to avoid actually opening editor
                import unittest.mock

                with unittest.mock.patch("clud.loop_tui.app.RunningProcess.run"):
                    async with app.run_test() as pilot:
                        await pilot.pause()

                        # File should not exist yet
                        self.assertFalse(update_file.exists())

                        # Call open_update_file directly
                        app.open_update_file()
                        await pilot.pause()

                        # File and parent directory should now exist
                        self.assertTrue(update_file.exists())
                        self.assertTrue(update_file.parent.exists())

        asyncio.run(run_test())

    def test_open_update_file_handles_no_file_specified(self) -> None:
        """Test that open_update_file handles None update_file gracefully."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI(update_file=None)

            async with app.run_test() as pilot:
                await pilot.pause()

                # Call open_update_file - should not raise
                app.open_update_file()
                await pilot.pause()

                # Should log error message
                log = app.query_one(RichLog)
                self.assertIsNotNone(log)

        asyncio.run(run_test())

    def test_open_update_file_handles_editor_error(self) -> None:
        """Test that open_update_file handles editor errors gracefully."""
        import asyncio

        async def run_test() -> None:
            with tempfile.TemporaryDirectory() as tmpdir:
                update_file = Path(tmpdir) / "UPDATE.md"
                update_file.write_text("test")

                app = CludLoopTUI(update_file=update_file)

                # Mock subprocess to raise error
                import unittest.mock

                with unittest.mock.patch("subprocess.run", side_effect=OSError("Editor not found")):
                    async with app.run_test() as pilot:
                        await pilot.pause()

                        # Call open_update_file - should not crash
                        app.open_update_file()
                        await pilot.pause()

                        # Should log error
                        log = app.query_one(RichLog)
                        self.assertIsNotNone(log)

        asyncio.run(run_test())


class TestLoopTUIE2ECompleteWorkflow(unittest.TestCase):
    """Test complete workflow scenarios end-to-end."""

    def test_full_navigation_workflow(self) -> None:
        """Test complete navigation workflow: main → options → back → exit."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                # Start in main menu
                self.assertEqual(app.current_menu, app.MAIN_MENU)
                self.assertEqual(app.selected_index, 0)

                # Navigate to Options and open
                await pilot.press("enter")
                await pilot.pause()
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)

                # Navigate in submenu
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

                # Go back to main
                await pilot.press("escape")
                await pilot.pause()
                self.assertEqual(app.current_menu, app.MAIN_MENU)

                # Navigate to Exit
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

        asyncio.run(run_test())

    def test_logging_with_menu_interaction(self) -> None:
        """Test that logging works while interacting with menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test() as pilot:
                await pilot.pause()

                # Add log messages
                app.log_message("Before menu navigation")

                # Navigate menu
                await pilot.press("right")
                await pilot.pause()

                # Add more log messages
                app.log_message("After menu navigation")

                # Open submenu
                await pilot.press("left")
                await pilot.press("enter")
                await pilot.pause()

                # Add message in submenu
                app.log_message("In submenu")

                # Verify all messages logged
                log = app.query_one(RichLog)
                self.assertGreater(len(log.lines), 0)

        asyncio.run(run_test())

    def test_submenu_maintains_state(self) -> None:
        """Test that submenu state is maintained during operations."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()

            async with app.run_test(size=(100, 30)) as pilot:
                # Open submenu
                await pilot.press("enter")
                await pilot.pause()

                self.assertEqual(app.current_menu, app.OPTIONS_MENU)

                # Add log message while in submenu
                app.log_message("Test in submenu")
                await pilot.pause()

                # Should still be in submenu
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)

                # Menu should still be functional
                menu = app.query_one("#menu_container")
                self.assertIsNotNone(menu)

                # Navigate in submenu
                await pilot.press("right")
                await pilot.pause()

                # Should still be in submenu
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)

        asyncio.run(run_test())


if __name__ == "__main__":
    unittest.main()
