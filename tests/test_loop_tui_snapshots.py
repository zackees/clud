"""Snapshot tests for loop TUI layout and rendering."""

from pathlib import Path
from typing import Any

from clud.loop_tui.app import CludLoopTUI


def test_initial_layout(snap_compare: Any) -> None:
    """Test initial TUI layout matches expected design."""
    app = CludLoopTUI()

    # Run app in test mode and compare snapshot
    assert snap_compare(app, terminal_size=(100, 30))


def test_menu_selection_first_item(snap_compare: Any) -> None:
    """Test menu selection visual appearance on first item."""
    app = CludLoopTUI()

    # First item (Options) should be selected by default
    assert snap_compare(app, terminal_size=(100, 30))


def test_menu_selection_second_item(snap_compare: Any) -> None:
    """Test menu selection visual appearance on second item."""
    app = CludLoopTUI()

    # Run app and press right to select second item
    assert snap_compare(app, press=["right"], terminal_size=(100, 30))


def test_options_submenu(snap_compare: Any) -> None:
    """Test options submenu layout."""
    app = CludLoopTUI()

    # Open options menu with Enter key
    assert snap_compare(app, press=["enter"], terminal_size=(100, 30))


def test_options_submenu_navigation(snap_compare: Any) -> None:
    """Test navigation within options submenu."""
    app = CludLoopTUI()

    # Open options menu and navigate to second item
    assert snap_compare(app, press=["enter", "right"], terminal_size=(100, 30))


def test_small_terminal(snap_compare: Any) -> None:
    """Test layout in small terminal (minimum size)."""
    app = CludLoopTUI()

    # Test at minimum recommended size
    assert snap_compare(app, terminal_size=(80, 20))


def test_large_terminal(snap_compare: Any) -> None:
    """Test layout in large terminal."""
    app = CludLoopTUI()

    # Test at large size
    assert snap_compare(app, terminal_size=(150, 40))


def test_with_log_messages(snap_compare: Any) -> None:
    """Test layout with some log messages in output area."""

    def run_before(pilot: Any) -> None:
        """Add some log messages before snapshot."""
        app = pilot.app
        if isinstance(app, CludLoopTUI):
            app.log_message("Test message 1")
            app.log_message("Test message 2")
            app.log_message("Test message 3")

    app = CludLoopTUI()

    # Run with callback to add messages
    assert snap_compare(app, run_before=run_before, terminal_size=(100, 30))


def test_back_to_main_menu(snap_compare: Any) -> None:
    """Test returning to main menu from options submenu."""
    app = CludLoopTUI()

    # Open options, then go back
    assert snap_compare(
        app,
        press=["enter", "enter"],
        terminal_size=(100, 30),  # Enter options  # Select Back
    )


def test_keyboard_navigation_wrap_around(snap_compare: Any) -> None:
    """Test menu wraps around when navigating past the end."""
    app = CludLoopTUI()

    # Navigate right twice (wraps to first item)
    assert snap_compare(app, press=["right", "right"], terminal_size=(100, 30))


def test_with_update_file_path(snap_compare: Any) -> None:
    """Test TUI with update file path specified."""
    import tempfile

    with tempfile.TemporaryDirectory() as tmpdir:
        update_file = Path(tmpdir) / "UPDATE.md"
        app = CludLoopTUI(update_file=update_file)

        # Should render normally even with update_file set
        assert snap_compare(app, terminal_size=(100, 30))


def test_header_text(snap_compare: Any) -> None:
    """Test that header displays correct text."""
    app = CludLoopTUI()

    # Header should say "clud --loop"
    assert snap_compare(app, terminal_size=(100, 30))


def test_help_text_visible(snap_compare: Any) -> None:
    """Test that help text is visible in menu area."""
    app = CludLoopTUI()

    # Help text should be visible
    assert snap_compare(app, terminal_size=(100, 30))


def test_vertical_resize(snap_compare: Any) -> None:
    """Test layout adapts to vertical terminal resize."""
    app = CludLoopTUI()

    # Test with tall terminal
    assert snap_compare(app, terminal_size=(100, 50))


def test_horizontal_resize(snap_compare: Any) -> None:
    """Test layout adapts to horizontal terminal resize."""
    app = CludLoopTUI()

    # Test with wide terminal
    assert snap_compare(app, terminal_size=(200, 30))
