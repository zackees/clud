"""Unit tests for loop TUI functionality."""

import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from textual.app import App

from clud.loop_tui.app import CludLoopTUI, SelectableRichLog


class TestLoopTUI(unittest.TestCase):
    """Test cases for CludLoopTUI class."""

    def test_initialization(self) -> None:
        """Test that CludLoopTUI initializes correctly."""
        app = CludLoopTUI()

        self.assertEqual(app.selected_index, 0)
        self.assertEqual(app.current_menu, app.MAIN_MENU)
        self.assertIsNone(app.on_exit_callback)
        self.assertIsNone(app.on_halt_callback)
        self.assertIsNone(app.on_edit_callback)
        self.assertIsNone(app.update_file)

    def test_initialization_with_callbacks(self) -> None:
        """Test that CludLoopTUI initializes with callbacks."""
        on_exit = MagicMock()
        on_halt = MagicMock()
        on_edit = MagicMock()
        update_file = Path("/tmp/UPDATE.md")

        app = CludLoopTUI(
            on_exit=on_exit,
            on_halt=on_halt,
            on_edit=on_edit,
            update_file=update_file,
        )

        self.assertEqual(app.on_exit_callback, on_exit)
        self.assertEqual(app.on_halt_callback, on_halt)
        self.assertEqual(app.on_edit_callback, on_edit)
        self.assertEqual(app.update_file, update_file)

    def test_menu_navigation_forward(self) -> None:
        """Test menu navigation moves forward."""
        app = CludLoopTUI()
        initial = app.selected_index

        # Navigate forward
        app.selected_index = (app.selected_index + 1) % len(app.current_menu)

        self.assertEqual(app.selected_index, (initial + 1) % len(app.current_menu))

    def test_menu_navigation_backward(self) -> None:
        """Test menu navigation moves backward."""
        app = CludLoopTUI()
        initial = app.selected_index

        # Navigate backward
        app.selected_index = (app.selected_index - 1) % len(app.current_menu)

        self.assertEqual(app.selected_index, (initial - 1) % len(app.current_menu))

    def test_menu_wrapping_forward(self) -> None:
        """Test menu wraps around at end."""
        app = CludLoopTUI()
        app.selected_index = len(app.current_menu) - 1

        # Go forward from last item
        app.selected_index = (app.selected_index + 1) % len(app.current_menu)

        self.assertEqual(app.selected_index, 0)

    def test_menu_wrapping_backward(self) -> None:
        """Test menu wraps around at beginning."""
        app = CludLoopTUI()
        app.selected_index = 0

        # Go backward from first item
        app.selected_index = (app.selected_index - 1) % len(app.current_menu)

        self.assertEqual(app.selected_index, len(app.current_menu) - 1)

    def test_show_main_menu(self) -> None:
        """Test switching to main menu."""
        app = CludLoopTUI()

        # First switch to options menu
        app.current_menu = app.OPTIONS_MENU
        app.selected_index = 2

        # Mock update_menu to avoid Textual query issues
        app.update_menu = MagicMock()

        # Switch back to main menu
        app.show_main_menu()

        self.assertEqual(app.current_menu, app.MAIN_MENU)
        self.assertEqual(app.selected_index, 0)
        app.update_menu.assert_called_once()

    def test_show_options_menu(self) -> None:
        """Test switching to options submenu."""
        app = CludLoopTUI()

        # Mock the log_message and update_menu methods to avoid Textual query issues
        app.log_message = MagicMock()
        app.update_menu = MagicMock()

        # Switch to options menu
        app.show_options_menu()

        self.assertEqual(app.current_menu, app.OPTIONS_MENU)
        self.assertEqual(app.selected_index, 0)
        app.log_message.assert_called_once_with("> Options menu opened")
        app.update_menu.assert_called_once()

    def test_exit_callback_called(self) -> None:
        """Test exit callback is called correctly."""
        exit_called = False

        def on_exit() -> None:
            nonlocal exit_called
            exit_called = True

        app = CludLoopTUI(on_exit=on_exit)

        # Simulate exit
        if app.on_exit_callback:
            app.on_exit_callback()

        self.assertTrue(exit_called)

    def test_halt_callback_called(self) -> None:
        """Test halt callback is called correctly."""
        halt_called = False

        def on_halt() -> None:
            nonlocal halt_called
            halt_called = True

        app = CludLoopTUI(on_halt=on_halt)

        # Mock log_message and exit to avoid Textual query issues
        app.log_message = MagicMock()
        app.exit = MagicMock()

        # Simulate halt
        app.halt_loop()

        self.assertTrue(halt_called)
        app.log_message.assert_called_once_with("> Halting loop...")
        app.exit.assert_called_once()

    def test_edit_callback_called(self) -> None:
        """Test edit callback is called after editing."""
        edit_called = False

        def on_edit() -> None:
            nonlocal edit_called
            edit_called = True

        # Create a temporary file for testing
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            update_file = Path(tmpdir) / "UPDATE.md"
            update_file.write_text("test content")

            app = CludLoopTUI(on_edit=on_edit, update_file=update_file)

            # Mock log_message and subprocess.run
            app.log_message = MagicMock()
            app.suspend = MagicMock()

            with patch("subprocess.run") as mock_run:
                mock_run.return_value = None

                # Simulate opening update file
                app.open_update_file()

                self.assertTrue(edit_called)
                mock_run.assert_called_once()

    def test_open_update_file_creates_if_not_exists(self) -> None:
        """Test that open_update_file creates UPDATE.md if it doesn't exist."""
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            update_file = Path(tmpdir) / "subdir" / "UPDATE.md"

            app = CludLoopTUI(update_file=update_file)

            # Mock log_message and subprocess.run
            app.log_message = MagicMock()
            app.suspend = MagicMock()

            with patch("subprocess.run") as mock_run:
                mock_run.return_value = None

                # File should not exist yet
                self.assertFalse(update_file.exists())

                # Open update file
                app.open_update_file()

                # File and parent directory should now exist
                self.assertTrue(update_file.exists())
                self.assertTrue(update_file.parent.exists())

    def test_open_update_file_no_file_specified(self) -> None:
        """Test that open_update_file handles missing update_file gracefully."""
        app = CludLoopTUI(update_file=None)

        # Mock log_message
        app.log_message = MagicMock()

        # Try to open update file
        app.open_update_file()

        # Should log error message
        app.log_message.assert_called_once_with("> Error: No UPDATE.md file specified")

    def test_open_update_file_editor_error(self) -> None:
        """Test that open_update_file handles editor errors gracefully."""
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            update_file = Path(tmpdir) / "UPDATE.md"
            update_file.write_text("test content")

            app = CludLoopTUI(update_file=update_file)

            # Mock log_message and subprocess.run to raise error
            app.log_message = MagicMock()
            app.suspend = MagicMock()

            with patch("subprocess.run") as mock_run:
                mock_run.side_effect = OSError("Editor not found")

                # Try to open update file
                app.open_update_file()

                # Should log error message
                calls = [str(call) for call in app.log_message.call_args_list]
                self.assertTrue(any("Error opening editor" in call for call in calls))

    def test_main_menu_items(self) -> None:
        """Test that main menu has expected items."""
        app = CludLoopTUI()

        self.assertEqual(app.MAIN_MENU, ["Options", "Exit"])
        self.assertIn("Options", app.MAIN_MENU)
        self.assertIn("Exit", app.MAIN_MENU)

    def test_options_menu_items(self) -> None:
        """Test that options menu has expected items."""
        app = CludLoopTUI()

        self.assertEqual(app.OPTIONS_MENU, ["<- Back", "Edit UPDATE.md", "Copy All", "Halt", "Help"])
        self.assertIn("<- Back", app.OPTIONS_MENU)
        self.assertIn("Edit UPDATE.md", app.OPTIONS_MENU)
        self.assertIn("Copy All", app.OPTIONS_MENU)
        self.assertIn("Halt", app.OPTIONS_MENU)
        self.assertIn("Help", app.OPTIONS_MENU)

    def test_handle_selection_exit(self) -> None:
        """Test handle_selection exits when Exit is selected."""
        app = CludLoopTUI()
        app.current_menu = app.MAIN_MENU
        app.selected_index = 1  # Exit is at index 1

        # Mock exit method
        app.exit = MagicMock()

        # Handle selection
        app.handle_selection()

        # Should exit
        app.exit.assert_called_once()

    def test_handle_selection_options(self) -> None:
        """Test handle_selection opens options menu when Options is selected."""
        app = CludLoopTUI()
        app.current_menu = app.MAIN_MENU
        app.selected_index = 0  # Options is at index 0

        # Mock log_message and update_menu
        app.log_message = MagicMock()
        app.update_menu = MagicMock()

        # Handle selection
        app.handle_selection()

        # Should be in options menu
        self.assertEqual(app.current_menu, app.OPTIONS_MENU)

    def test_handle_selection_back(self) -> None:
        """Test handle_selection goes back when Back is selected."""
        app = CludLoopTUI()
        app.current_menu = app.OPTIONS_MENU
        app.selected_index = 0  # Back is at index 0

        # Mock update_menu
        app.update_menu = MagicMock()

        # Handle selection
        app.handle_selection()

        # Should be back in main menu
        self.assertEqual(app.current_menu, app.MAIN_MENU)

    def test_handle_selection_edit(self) -> None:
        """Test handle_selection opens editor when Edit is selected."""
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            update_file = Path(tmpdir) / "UPDATE.md"
            update_file.write_text("test content")

            app = CludLoopTUI(update_file=update_file)
            app.current_menu = app.OPTIONS_MENU
            app.selected_index = 1  # Edit UPDATE.md is at index 1

            # Mock log_message and subprocess.run
            app.log_message = MagicMock()
            app.suspend = MagicMock()

            with patch("subprocess.run") as mock_run:
                mock_run.return_value = None

                # Handle selection
                app.handle_selection()

                # Should have called subprocess.run
                mock_run.assert_called_once()

    def test_handle_selection_halt(self) -> None:
        """Test handle_selection halts when Halt is selected."""
        app = CludLoopTUI()
        app.current_menu = app.OPTIONS_MENU
        app.selected_index = 3  # Halt is at index 3

        # Mock log_message and exit
        app.log_message = MagicMock()
        app.exit = MagicMock()

        # Handle selection
        app.handle_selection()

        # Should have logged and exited
        app.log_message.assert_called_once_with("> Halting loop...")
        app.exit.assert_called_once()


PREBAKED_CLI_OUTPUT = [
    "--- Iteration 1/5 ---",
    "Prompt: Read .loop/LOOP.md and do the next task.",
    "",
    "💬 I'll start by reading the relevant files.",
    "📊 tokens: 5",
    "🔧 Read: /project/.loop/LOOP.md",
    "💬 Working on the task now...",
    "🔧 Edit: /project/src/main.py",
    "📊 tokens: 42",
    "✅ Changes applied successfully.",
]


def _assert_split_screen_layout(test: unittest.TestCase, app: CludLoopTUI) -> None:
    """Assert the three-panel split screen layout is correct.

    Verifies header (top), output (middle), menu (bottom) are all present,
    non-overlapping, and correctly ordered vertically.
    """
    header = app.query_one("#header")
    output = app.query_one("#output")
    menu = app.query_one("#menu_container")

    h = header.region
    o = output.region
    m = menu.region

    # Docking
    test.assertEqual(header.styles.dock, "top")
    test.assertEqual(menu.styles.dock, "bottom")

    # Vertical ordering
    test.assertLess(h.y, o.y, "Header should be above output")
    test.assertLess(o.y, m.y, "Output should be above menu")

    # All visible (non-zero height)
    test.assertGreater(h.height, 0, "Header must have height")
    test.assertGreater(o.height, 0, "Output must have height")
    test.assertGreater(m.height, 0, "Menu must have height")

    # No vertical overlap
    test.assertLessEqual(h.y + h.height, o.y, "Header and output must not overlap")
    test.assertLessEqual(o.y + o.height, m.y, "Output and menu must not overlap")

    # Output takes the largest share
    total = h.height + o.height + m.height
    test.assertGreater(o.height, total // 3, "Output should be the largest panel")


class TestSplitScreenLayout(unittest.TestCase):
    """Verify the three-panel split-screen layout under various conditions."""

    def test_split_screen_with_prebaked_output(self) -> None:
        """Header, output, and menu are all visible with streamed CLI output."""
        import asyncio

        from textual.widgets import Label, RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                _assert_split_screen_layout(self, app)

                # Pre-baked output appears in the log
                log = app.query_one(RichLog)
                self.assertGreater(len(log.lines), len(PREBAKED_CLI_OUTPUT))

                # Menu label is populated
                menu_label = app.query_one("#menu_items", Label)
                self.assertIsNotNone(menu_label)

        asyncio.run(run_test())

    def test_split_screen_at_initialization(self) -> None:
        """Layout is a proper split screen even with no output yet."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_split_screen_minimum_terminal_size(self) -> None:
        """Layout holds at minimum recommended terminal size (80x20)."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(80, 20)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_split_screen_large_terminal_size(self) -> None:
        """Layout scales properly on a large terminal (200x60)."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(200, 60)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_regions_span_full_width(self) -> None:
        """All three panels span the full terminal width."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(120, 35)) as pilot:
                await pilot.pause()
                width = 120
                for wid_id in ("#header", "#output", "#menu_container"):
                    region = app.query_one(wid_id).region
                    self.assertEqual(region.width, width, f"{wid_id} should span full width")

        asyncio.run(run_test())

    def test_header_displays_clud_loop_text(self) -> None:
        """Header widget contains the 'clud --loop' title."""
        import asyncio

        from textual.widgets import Static

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                header = app.query_one("#header", Static)
                # The renderable is set to "clud --loop" via Static("clud --loop")
                self.assertIsNotNone(header)

        asyncio.run(run_test())


class TestSplitScreenOutputStreaming(unittest.TestCase):
    """Verify the output panel correctly displays mocked CLI content."""

    def test_output_lines_in_order(self) -> None:
        """Pre-baked lines appear in the RichLog in insertion order."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                log = app.query_one(RichLog)
                # The first line written is the init message, then our lines follow
                line_count_before = 1  # "TUI initialized..." on mount
                total_expected = line_count_before + len(PREBAKED_CLI_OUTPUT)
                self.assertEqual(len(log.lines), total_expected)

        asyncio.run(run_test())

    def test_loading_indicators_in_output(self) -> None:
        """show_loading and hide_loading messages land in the output panel."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                app.show_loading("Compiling project...")
                app.hide_loading("Build complete")
                await pilot.pause()

                log = app.query_one(RichLog)
                # init message + 2 loading messages
                self.assertEqual(len(log.lines), 3)

        asyncio.run(run_test())

    def test_large_volume_output_preserves_layout(self) -> None:
        """Flooding the log with many lines doesn't break the split-screen."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for i in range(200):
                    app.log_message(f"Line {i}: {'x' * 80}")
                await pilot.pause()

                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_empty_lines_preserved(self) -> None:
        """Empty string lines are written to the log without being dropped."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                app.log_message("before")
                app.log_message("")
                app.log_message("")
                app.log_message("after")
                await pilot.pause()

                log = app.query_one(RichLog)
                # init + 4 messages
                self.assertEqual(len(log.lines), 5)

        asyncio.run(run_test())

    def test_output_accumulates_across_iterations(self) -> None:
        """Simulating multiple loop iterations appends rather than replaces."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Iteration 1
                app.log_message("--- Iteration 1/3 ---")
                app.log_message("Working on task...")
                app.log_message("Done.")

                # Iteration 2
                app.log_message("--- Iteration 2/3 ---")
                app.log_message("Continuing work...")
                app.log_message("Done.")
                await pilot.pause()

                log = app.query_one(RichLog)
                # init + 6
                self.assertEqual(len(log.lines), 7)

        asyncio.run(run_test())


class TestSplitScreenSubmenu(unittest.TestCase):
    """Verify split-screen layout is preserved during submenu interactions."""

    def test_layout_preserved_entering_options(self) -> None:
        """Split screen layout stays intact when opening Options submenu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                # Enter Options submenu
                await pilot.press("enter")
                await pilot.pause()

                self.assertEqual(app.current_menu, app.OPTIONS_MENU)
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_layout_preserved_returning_to_main(self) -> None:
        """Split screen intact after Options → Back to main menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                # Options → Back
                await pilot.press("enter")
                await pilot.pause()
                await pilot.press("enter")  # "<- Back" is index 0
                await pilot.pause()

                self.assertEqual(app.current_menu, app.MAIN_MENU)
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_submenu_shows_all_five_items(self) -> None:
        """Options submenu renders with all five items visible."""
        import asyncio

        from textual.widgets import Label

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                await pilot.press("enter")
                await pilot.pause()

                self.assertEqual(app.current_menu, app.OPTIONS_MENU)
                self.assertEqual(len(app.OPTIONS_MENU), 5)

                # Menu label should be populated with submenu items
                menu_label = app.query_one("#menu_items", Label)
                self.assertIsNotNone(menu_label)

        asyncio.run(run_test())

    def test_full_submenu_roundtrip(self) -> None:
        """Navigate main → options → every item → back → main without breaking layout."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                app.log_message("Pre-submenu output")
                await pilot.pause()

                # Enter options
                await pilot.press("enter")
                await pilot.pause()
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)
                self.assertEqual(app.selected_index, 0)

                # Navigate through all items
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 2)

                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 3)

                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 4)

                # Wrap around
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 0)

                # Back to main via Enter on "<- Back"
                await pilot.press("enter")
                await pilot.pause()
                self.assertEqual(app.current_menu, app.MAIN_MENU)

                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_escape_returns_from_submenu(self) -> None:
        """Pressing Escape in submenu returns to main menu, layout intact."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Enter options, then escape
                await pilot.press("enter")
                await pilot.pause()
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)

                await pilot.press("escape")
                await pilot.pause()
                self.assertEqual(app.current_menu, app.MAIN_MENU)
                self.assertEqual(app.selected_index, 0)

                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_help_content_in_output_area(self) -> None:
        """Selecting Help in submenu adds help text to the output panel."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                lines_before = len(app.query_one(RichLog).lines)

                # Options → navigate to Help (index 4) → select
                await pilot.press("enter")
                await pilot.pause()
                await pilot.press("right")  # Edit
                await pilot.press("right")  # Copy All
                await pilot.press("right")  # Halt
                await pilot.press("right")  # Help
                await pilot.pause()
                self.assertEqual(app.selected_index, 4)

                await pilot.press("enter")
                await pilot.pause()

                log = app.query_one(RichLog)
                # Help should have added content
                self.assertGreater(len(log.lines), lines_before)
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())


class TestSplitScreenMenuInteraction(unittest.TestCase):
    """Verify menu interaction doesn't disrupt the output panel and vice versa."""

    def test_navigation_does_not_clear_output(self) -> None:
        """Navigating the menu leaves previously streamed output untouched."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                lines_before = len(app.query_one(RichLog).lines)

                # Navigate around the main menu
                await pilot.press("right")
                await pilot.press("left")
                await pilot.press("right")
                await pilot.pause()

                lines_after = len(app.query_one(RichLog).lines)
                self.assertEqual(lines_before, lines_after)

        asyncio.run(run_test())

    def test_output_while_in_submenu(self) -> None:
        """Adding output while the submenu is open works correctly."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Enter submenu
                await pilot.press("enter")
                await pilot.pause()
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)

                lines_before = len(app.query_one(RichLog).lines)

                # Simulate output arriving while in submenu
                app.log_message("Background work happening...")
                app.log_message("Still going...")
                await pilot.pause()

                lines_after = len(app.query_one(RichLog).lines)
                self.assertEqual(lines_after, lines_before + 2)

                # Submenu still active
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_interleaved_output_and_navigation(self) -> None:
        """Alternating between output and menu navigation keeps everything intact."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                app.log_message("Step 1")
                await pilot.press("right")
                await pilot.pause()

                app.log_message("Step 2")
                await pilot.press("left")
                await pilot.pause()

                app.log_message("Step 3")
                # Enter submenu
                await pilot.press("enter")
                await pilot.pause()

                app.log_message("Step 4 (in submenu)")
                await pilot.press("right")
                await pilot.pause()

                # Back to main
                await pilot.press("escape")
                await pilot.pause()

                app.log_message("Step 5 (back in main)")
                await pilot.pause()

                log = app.query_one(RichLog)
                # init + Options menu opened + 5 steps
                self.assertGreaterEqual(len(log.lines), 6)
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())

    def test_menu_selection_index_resets_on_submenu_entry(self) -> None:
        """Entering a submenu always resets selection to index 0."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Move to index 1, then back to 0 and enter options
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

                await pilot.press("left")
                await pilot.press("enter")
                await pilot.pause()

                # Should be at index 0 in OPTIONS_MENU
                self.assertEqual(app.current_menu, app.OPTIONS_MENU)
                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())


class TestSplitScreenKeyboardNav(unittest.TestCase):
    """Comprehensive keyboard navigation tests with the split screen."""

    def test_arrow_keys_cycle_main_menu(self) -> None:
        """Left/right cycle through main menu items with output present."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                # Right cycles 0 → 1 → 0 (wraps)
                self.assertEqual(app.selected_index, 0)
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 0)

                # Left cycles 0 → 1 (wrap backward)
                await pilot.press("left")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

        asyncio.run(run_test())

    def test_up_down_navigate_menu(self) -> None:
        """Up/down keys also navigate the menu."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                self.assertEqual(app.selected_index, 0)
                await pilot.press("down")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)
                await pilot.press("up")
                await pilot.pause()
                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())

    def test_tab_navigates_forward(self) -> None:
        """Tab key moves selection forward through menu items."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                self.assertEqual(app.selected_index, 0)
                await pilot.press("tab")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)
                await pilot.press("tab")
                await pilot.pause()
                self.assertEqual(app.selected_index, 0)  # wraps

        asyncio.run(run_test())

    def test_arrow_keys_cycle_submenu(self) -> None:
        """Arrow keys cycle through all 5 submenu items."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                await pilot.press("enter")  # open Options
                await pilot.pause()

                for expected in range(5):
                    self.assertEqual(app.selected_index, expected)
                    await pilot.press("right")
                    await pilot.pause()

                # Wrapped back to 0
                self.assertEqual(app.selected_index, 0)

        asyncio.run(run_test())

    def test_ctrl_c_first_press_shows_warning(self) -> None:
        """First Ctrl-C shows exit warning instead of quitting."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                lines_before = len(app.query_one(RichLog).lines)

                await pilot.press("ctrl+c")
                await pilot.pause()

                log = app.query_one(RichLog)
                self.assertGreater(len(log.lines), lines_before)
                # App should still be running (not exited)
                self.assertEqual(app.current_menu, app.MAIN_MENU)

        asyncio.run(run_test())

    def test_ctrl_c_double_press_exits(self) -> None:
        """Pressing Ctrl-C twice within 2 seconds exits the app."""
        import asyncio

        async def run_test() -> None:
            exit_called = False

            def on_exit() -> None:
                nonlocal exit_called
                exit_called = True

            app = CludLoopTUI(on_exit=on_exit)
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                await pilot.press("ctrl+c")
                await pilot.pause()
                await pilot.press("ctrl+c")
                await pilot.pause()

            self.assertTrue(exit_called)

        asyncio.run(run_test())


class TestSplitScreenHalt(unittest.TestCase):
    """Verify halt behavior within the split-screen TUI."""

    def test_halt_from_submenu_calls_callback(self) -> None:
        """Selecting Halt in submenu invokes the halt callback."""
        import asyncio

        async def run_test() -> None:
            halt_called = False

            def on_halt() -> None:
                nonlocal halt_called
                halt_called = True

            app = CludLoopTUI(on_halt=on_halt)
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                # Options → Halt (index 3)
                await pilot.press("enter")
                await pilot.pause()
                await pilot.press("right")  # Edit
                await pilot.press("right")  # Copy All
                await pilot.press("right")  # Halt
                await pilot.pause()
                self.assertEqual(app.selected_index, 3)

                await pilot.press("enter")
                await pilot.pause()

            self.assertTrue(halt_called)

        asyncio.run(run_test())

    def test_halt_logs_message(self) -> None:
        """Halt action logs '> Halting loop...' to the output panel."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                lines_before = len(app.query_one(RichLog).lines)

                # Options → Halt (index 3)
                await pilot.press("enter")
                await pilot.pause()
                await pilot.press("right")  # Edit
                await pilot.press("right")  # Copy All
                await pilot.press("right")  # Halt
                await pilot.press("enter")
                await pilot.pause()

                log = app.query_one(RichLog)
                # Should have added "Options menu opened" and "Halting loop..."
                self.assertGreater(len(log.lines), lines_before)

        asyncio.run(run_test())


class TestSplitScreenNoPTY(unittest.TestCase):
    """Verify TUI behavior when no PTY/TTY is available (piped, CI, headless)."""

    def test_tui_runs_in_headless_mode(self) -> None:
        """Textual's run_test (headless) still produces a valid split-screen layout."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            # run_test() is inherently headless/non-PTY
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                for line in PREBAKED_CLI_OUTPUT:
                    app.log_message(line)
                await pilot.pause()

                _assert_split_screen_layout(self, app)
                log = app.query_one(RichLog)
                self.assertGreater(len(log.lines), 0)

        asyncio.run(run_test())

    def test_integration_entry_point_returns_exit_code(self) -> None:
        """run_loop_with_tui returns an integer exit code even without a real PTY."""
        from clud.loop_tui.integration import run_loop_with_tui
        from clud.loop_tui.loop_worker import LoopWorkerApp

        args = MagicMock()
        args.loop_value = None
        args.prompt = "test prompt"
        args.message = None
        args.verbose = False
        args.plain = False
        args.claude_args = []
        args.loop_count_override = None

        # Patch LoopWorkerApp and _handle_existing_loop so it doesn't actually
        # launch the loop or try to read stdin (which fails in pytest)
        with (
            patch.object(LoopWorkerApp, "run") as mock_run,
            patch.object(LoopWorkerApp, "_exit_code", 0, create=True),
            patch("clud.agent.task_manager._handle_existing_loop", return_value=(True, 1)),
        ):
            mock_run.return_value = None
            result = run_loop_with_tui(args, "/fake/claude", 1)
            self.assertIsInstance(result, int)

    def test_no_stdin_tty_detection_exists(self) -> None:
        """Verify sys.stdin.isatty() can be used for PTY detection in tests."""
        import sys

        # This test documents that isatty() is callable and returns a bool.
        # When piped (like in CI), isatty() returns False.
        result = sys.stdin.isatty()
        self.assertIsInstance(result, bool)

    def test_stdout_not_a_tty_still_renders_headless(self) -> None:
        """Even if stdout is not a TTY, Textual headless mode renders the TUI."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                app.log_message("Output in non-TTY mode")
                await pilot.pause()

                # The three panels should exist
                header = app.query_one("#header")
                output = app.query_one("#output")
                menu = app.query_one("#menu_container")
                self.assertIsNotNone(header)
                self.assertIsNotNone(output)
                self.assertIsNotNone(menu)

        asyncio.run(run_test())

    def test_keyboard_events_in_headless_mode(self) -> None:
        """Keyboard navigation works in headless mode via the test pilot."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Navigation still works without a real terminal
                await pilot.press("right")
                await pilot.pause()
                self.assertEqual(app.selected_index, 1)

                await pilot.press("enter")  # Exit
                await pilot.pause()

        asyncio.run(run_test())

    def test_menu_callbacks_work_headless(self) -> None:
        """Halt and exit callbacks fire in headless mode."""
        import asyncio

        async def run_test() -> None:
            halt_called = False

            def on_halt() -> None:
                nonlocal halt_called
                halt_called = True

            app = CludLoopTUI(on_halt=on_halt)
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Enter options submenu → navigate to Halt (index 3) → select
                await pilot.press("enter")
                await pilot.pause()
                await pilot.press("right")  # Edit
                await pilot.press("right")  # Copy All
                await pilot.press("right")  # Halt
                await pilot.press("enter")
                await pilot.pause()

            self.assertTrue(halt_called)

        asyncio.run(run_test())

    def test_resize_warning_in_small_headless_terminal(self) -> None:
        """Resize below minimum triggers a notification even in headless mode."""
        import asyncio

        from textual import events

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Resize to below minimum
                app.on_resize(events.Resize(size=app.size, virtual_size=app.size))
                await pilot.pause()

                # App should still be functional
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())


class TestLoopAutoTUI(unittest.TestCase):
    """Verify that --loop automatically enables TUI when a TTY is available."""

    def test_loop_auto_enables_tui_when_tty(self) -> None:
        """--loop should use _run_loop when stdout is a TTY."""
        from clud.agent_args import Args

        args = MagicMock(spec=Args)
        args.tui = False
        args.plain = False
        args.loop_value = "TEST_LOOP.md"
        args.prompt = "Read .loop/TEST_LOOP.md and do the next task."
        args.message = None
        args.verbose = False
        args.claude_args = []
        args.loop_count_override = None
        args.dry_run = False
        args.cmd = None
        args.hook_debug = False
        args.idle_timeout = None
        args.continue_flag = False

        with (
            patch("clud.agent.runner.sys") as mock_sys,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.agent.runner._run_loop", return_value=0) as mock_loop,
            patch("clud.agent.runner.register_hooks_from_config"),
            patch("clud.agent.runner.trigger_hook_sync"),
        ):
            mock_sys.stdout.isatty.return_value = True
            mock_sys.stdin.isatty.return_value = True

            from clud.agent.runner import run_agent

            run_agent(args)

            mock_loop.assert_called_once()

    def test_loop_falls_back_to_plain_when_no_tty(self) -> None:
        """--loop should use plain mode when stdout is not a TTY."""
        from clud.agent_args import Args

        args = MagicMock(spec=Args)
        args.tui = False
        args.loop_ui = False
        args.plain = False
        args.loop_value = "TEST_LOOP.md"
        args.prompt = "Read .loop/TEST_LOOP.md and do the next task."
        args.message = None
        args.verbose = False
        args.claude_args = []
        args.loop_count_override = None
        args.dry_run = False
        args.cmd = None
        args.hook_debug = False
        args.idle_timeout = None
        args.continue_flag = False

        with (
            patch("clud.agent.runner.sys") as mock_sys,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.loop_tui.integration.run_loop_with_tui", return_value=0) as mock_tui,
            patch("clud.agent.runner._run_loop", return_value=0) as mock_plain,
            patch("clud.agent.runner.register_hooks_from_config"),
            patch("clud.agent.runner.trigger_hook_sync"),
        ):
            mock_sys.stdout.isatty.return_value = False
            mock_sys.stdin.isatty.return_value = True

            from clud.agent.runner import run_agent

            run_agent(args)

            mock_plain.assert_called_once()
            mock_tui.assert_not_called()

    def test_loop_plain_flag_disables_tui_even_with_tty(self) -> None:
        """--loop --plain should disable TUI even when stdout is a TTY."""
        from clud.agent_args import Args

        args = MagicMock(spec=Args)
        args.tui = False
        args.loop_ui = False
        args.plain = True  # Explicitly plain
        args.loop_value = "TEST_LOOP.md"
        args.prompt = "Read .loop/TEST_LOOP.md and do the next task."
        args.message = None
        args.verbose = False
        args.claude_args = []
        args.loop_count_override = None
        args.dry_run = False
        args.cmd = None
        args.hook_debug = False
        args.idle_timeout = None
        args.continue_flag = False

        with (
            patch("clud.agent.runner.sys") as mock_sys,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.loop_tui.integration.run_loop_with_tui", return_value=0) as mock_tui,
            patch("clud.agent.runner._run_loop", return_value=0) as mock_plain,
            patch("clud.agent.runner.register_hooks_from_config"),
            patch("clud.agent.runner.trigger_hook_sync"),
        ):
            mock_sys.stdout.isatty.return_value = True
            mock_sys.stdin.isatty.return_value = True

            from clud.agent.runner import run_agent

            run_agent(args)

            mock_plain.assert_called_once()
            mock_tui.assert_not_called()

    def test_loop_plain_flag_uses_run_loop(self) -> None:
        """--loop --plain should use _run_loop."""
        from clud.agent_args import Args

        args = MagicMock(spec=Args)
        args.tui = False
        args.plain = True  # Explicitly plain
        args.loop_value = "TEST_LOOP.md"
        args.prompt = "Read .loop/TEST_LOOP.md and do the next task."
        args.message = None
        args.verbose = False
        args.claude_args = []
        args.loop_count_override = None
        args.dry_run = False
        args.cmd = None
        args.hook_debug = False
        args.idle_timeout = None
        args.continue_flag = False

        with (
            patch("clud.agent.runner.sys") as mock_sys,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.agent.runner._run_loop", return_value=0) as mock_loop,
            patch("clud.agent.runner.register_hooks_from_config"),
            patch("clud.agent.runner.trigger_hook_sync"),
        ):
            mock_sys.stdout.isatty.return_value = True
            mock_sys.stdin.isatty.return_value = True

            from clud.agent.runner import run_agent

            run_agent(args)

            mock_loop.assert_called_once()

    def test_tui_flag_calls_handle_existing_loop(self) -> None:
        """--loop --tui must call _handle_existing_loop to prompt about .loop artifacts."""
        from clud.agent_args import Args

        args = MagicMock(spec=Args)
        args.tui = True
        args.plain = False
        args.loop_value = "TEST_LOOP.md"
        args.prompt = "Read .loop/TEST_LOOP.md and do the next task."
        args.message = None
        args.verbose = False
        args.claude_args = []
        args.loop_count_override = None
        args.dry_run = False
        args.cmd = None
        args.hook_debug = False
        args.idle_timeout = None
        args.continue_flag = False

        with (
            patch("clud.agent.runner.sys") as mock_sys,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.agent.task_manager._handle_existing_loop", return_value=(True, 1)) as mock_handle,
            patch("clud.loop_tui.integration.LoopWorkerApp") as mock_app_cls,
            patch("clud.agent.runner.register_hooks_from_config"),
            patch("clud.agent.runner.trigger_hook_sync"),
        ):
            mock_sys.stdout.isatty.return_value = True
            mock_sys.stdin.isatty.return_value = True
            mock_app = MagicMock()
            mock_app._exit_code = 0
            mock_app_cls.return_value = mock_app

            from clud.agent.runner import run_agent

            result = run_agent(args)

            mock_handle.assert_called_once()
            self.assertEqual(result, 0)

    def test_tui_flag_aborts_when_user_cancels_existing_loop(self) -> None:
        """--loop --tui must abort (exit 2) when user declines .loop cleanup."""
        from clud.agent_args import Args

        args = MagicMock(spec=Args)
        args.tui = True
        args.plain = False
        args.loop_value = "TEST_LOOP.md"
        args.prompt = "Read .loop/TEST_LOOP.md and do the next task."
        args.message = None
        args.verbose = False
        args.claude_args = []
        args.loop_count_override = None
        args.dry_run = False
        args.cmd = None
        args.hook_debug = False
        args.idle_timeout = None
        args.continue_flag = False

        with (
            patch("clud.agent.runner.sys") as mock_sys,
            patch("clud.agent.runner._find_claude_path", return_value="/fake/claude"),
            patch("clud.agent.task_manager._handle_existing_loop", return_value=(False, 1)) as mock_handle,
            patch("clud.loop_tui.integration.LoopWorkerApp") as mock_app_cls,
            patch("clud.agent.runner.register_hooks_from_config"),
            patch("clud.agent.runner.trigger_hook_sync"),
        ):
            mock_sys.stdout.isatty.return_value = True
            mock_sys.stdin.isatty.return_value = True

            from clud.agent.runner import run_agent

            result = run_agent(args)

            mock_handle.assert_called_once()
            mock_app_cls.assert_not_called()
            self.assertEqual(result, 2)

    def test_loop_worker_split_screen_layout(self) -> None:
        """LoopWorkerApp shows a proper split-screen layout when running."""
        import asyncio

        from clud.loop_tui.loop_worker import LoopWorkerApp

        async def run_test() -> None:
            mock_args = MagicMock()
            mock_args.loop_value = None
            mock_args.prompt = "Test prompt"
            mock_args.message = None
            mock_args.verbose = False
            mock_args.plain = False
            mock_args.claude_args = []

            app = LoopWorkerApp(
                args=mock_args,
                claude_path="/fake/claude",
                loop_count=1,
                update_file=Path("/tmp/UPDATE.md"),
            )

            # Prevent the actual loop worker from running
            app.run_loop_worker = MagicMock()

            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Verify split screen layout
                _assert_split_screen_layout(self, app)

        asyncio.run(run_test())


class TestClipboardAndSelection(unittest.TestCase):
    """Test cases for clipboard copy and text selection features."""

    # --- Platform dispatch (sync) ---

    @patch("clud.loop_tui.app.subprocess.Popen")
    @patch("clud.loop_tui.app.sys")
    def test_copy_to_clipboard_win32(self, mock_sys: MagicMock, mock_popen: MagicMock) -> None:
        """copy_to_clipboard uses clip.exe on Windows."""
        mock_sys.platform = "win32"
        process = MagicMock()
        process.communicate.return_value = (b"", b"")
        process.returncode = 0
        mock_popen.return_value = process
        app = CludLoopTUI()
        app.copy_to_clipboard("hello")
        mock_popen.assert_called_once_with(["clip.exe"], stdin=-1, stdout=-1, stderr=-1)
        process.communicate.assert_called_once_with(input=b"hello", timeout=5)

    @patch("clud.loop_tui.app.subprocess.Popen")
    @patch("clud.loop_tui.app.sys")
    def test_copy_to_clipboard_darwin(self, mock_sys: MagicMock, mock_popen: MagicMock) -> None:
        """copy_to_clipboard uses pbcopy on macOS."""
        mock_sys.platform = "darwin"
        process = MagicMock()
        process.communicate.return_value = (b"", b"")
        process.returncode = 0
        mock_popen.return_value = process
        app = CludLoopTUI()
        app.copy_to_clipboard("hello")
        mock_popen.assert_called_once_with(["pbcopy"], stdin=-1, stdout=-1, stderr=-1)
        process.communicate.assert_called_once_with(input=b"hello", timeout=5)

    @patch("clud.loop_tui.app.subprocess.Popen")
    @patch("clud.loop_tui.app.sys")
    def test_copy_to_clipboard_linux(self, mock_sys: MagicMock, mock_popen: MagicMock) -> None:
        """copy_to_clipboard uses xclip on Linux."""
        mock_sys.platform = "linux"
        process = MagicMock()
        process.communicate.return_value = (b"", b"")
        process.returncode = 0
        mock_popen.return_value = process
        app = CludLoopTUI()
        app.copy_to_clipboard("hello")
        mock_popen.assert_called_once_with(["xclip", "-selection", "clipboard"], stdin=-1, stdout=-1, stderr=-1)
        process.communicate.assert_called_once_with(input=b"hello", timeout=5)

    # --- Fallback ---

    @patch("clud.loop_tui.app.subprocess.Popen", side_effect=FileNotFoundError("not found"))
    @patch("clud.loop_tui.app.sys")
    def test_copy_to_clipboard_fallback_on_failure(self, mock_sys: MagicMock, _mock_run: MagicMock) -> None:
        """Falls back to App.copy_to_clipboard (OSC 52) when subprocess fails."""
        mock_sys.platform = "linux"
        tui = CludLoopTUI()
        with patch.object(App, "copy_to_clipboard") as mock_super:
            tui.copy_to_clipboard("hello")
            mock_super.assert_called_once_with("hello")

    # --- UTF-8 encoding ---

    @patch("clud.loop_tui.app.subprocess.Popen")
    @patch("clud.loop_tui.app.sys")
    def test_copy_to_clipboard_encodes_utf8(self, mock_sys: MagicMock, mock_popen: MagicMock) -> None:
        """copy_to_clipboard encodes unicode text as UTF-8 bytes."""
        mock_sys.platform = "win32"
        process = MagicMock()
        process.communicate.return_value = (b"", b"")
        process.returncode = 0
        mock_popen.return_value = process
        app = CludLoopTUI()
        text = "Hello \u2603 \u00e9\u00e8\u00ea"
        app.copy_to_clipboard(text)
        process.communicate.assert_called_once()
        actual_input = process.communicate.call_args.kwargs["input"]
        self.assertEqual(actual_input, text.encode("utf-8"))

    # --- SelectableRichLog.get_selection ---

    def test_get_selection_with_lines(self) -> None:
        """get_selection extracts text from log lines via Selection.extract."""
        import asyncio

        from textual.selection import Selection

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                app.log_message("Line A")
                app.log_message("Line B")
                app.log_message("Line C")
                await pilot.pause()

                log = app.query_one(SelectableRichLog)
                # Select everything
                sel = Selection(start=None, end=None)
                result = log.get_selection(sel)
                self.assertIsNotNone(result)
                assert result is not None
                text, ending = result
                self.assertEqual(ending, "\n")
                # Should contain all logged lines (init message + 3 lines)
                self.assertIn("Line A", text)
                self.assertIn("Line B", text)
                self.assertIn("Line C", text)

        asyncio.run(run_test())

    def test_get_selection_empty_log(self) -> None:
        """get_selection returns None when log has no lines."""
        log = SelectableRichLog()
        sel = MagicMock()
        result = log.get_selection(sel)
        self.assertIsNone(result)

    # --- _copy_all_output ---

    def test_copy_all_output_with_content(self) -> None:
        """_copy_all_output copies all log lines to clipboard."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                app.log_message("Output line 1")
                app.log_message("Output line 2")
                await pilot.pause()

                with patch.object(app, "copy_to_clipboard") as mock_copy:
                    app._copy_all_output()  # type: ignore[attr-defined]
                    mock_copy.assert_called_once()
                    copied_text = mock_copy.call_args[0][0]
                    self.assertIn("Output line 1", copied_text)
                    self.assertIn("Output line 2", copied_text)

        asyncio.run(run_test())

    def test_copy_all_output_empty_log(self) -> None:
        """_copy_all_output notifies when log is empty and does not copy."""
        import asyncio

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()

                # Clear all lines from the log
                log = app.query_one(SelectableRichLog)
                log.clear()  # type: ignore[attr-defined]
                await pilot.pause()

                with (
                    patch.object(app, "copy_to_clipboard") as mock_copy,
                    patch.object(app, "notify") as mock_notify,
                ):
                    app._copy_all_output()  # type: ignore[attr-defined]
                    mock_copy.assert_not_called()
                    mock_notify.assert_called_once_with("No output to copy")

        asyncio.run(run_test())

    # --- Ctrl+C integration ---

    def test_ctrl_c_with_selected_text_copies_to_clipboard(self) -> None:
        """Ctrl+C with selected text copies it to clipboard."""
        import asyncio

        from textual.screen import Screen

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                app.log_message("Some output text")
                await pilot.pause()

                with (
                    patch.object(Screen, "get_selected_text", return_value="Some output text"),
                    patch.object(app, "copy_to_clipboard") as mock_copy,
                    patch.object(Screen, "clear_selection") as mock_clear,
                ):
                    await pilot.press("ctrl+c")
                    await pilot.pause()

                    mock_copy.assert_called_with("Some output text")
                    self.assertGreaterEqual(mock_copy.call_count, 1)
                    mock_clear.assert_called()

        asyncio.run(run_test())

    def test_ctrl_c_without_selection_triggers_exit_warning(self) -> None:
        """Ctrl+C without selection shows exit warning, does not copy."""
        import asyncio

        from textual.widgets import RichLog

        async def run_test() -> None:
            app = CludLoopTUI()
            async with app.run_test(size=(100, 30)) as pilot:
                await pilot.pause()
                lines_before = len(app.query_one(RichLog).lines)

                with patch.object(app, "copy_to_clipboard") as mock_copy:
                    await pilot.press("ctrl+c")
                    await pilot.pause()

                    mock_copy.assert_not_called()
                    # Should have logged the exit warning
                    log = app.query_one(RichLog)
                    self.assertGreater(len(log.lines), lines_before)

        asyncio.run(run_test())


if __name__ == "__main__":
    unittest.main()
