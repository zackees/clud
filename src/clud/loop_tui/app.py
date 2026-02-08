"""Main TUI application for clud --loop mode."""

import logging
import os
import subprocess
import sys
import time
from collections.abc import AsyncIterator, Callable
from pathlib import Path

from rich.text import Text
from textual import events, work
from textual.app import App, ComposeResult
from textual.containers import Container
from textual.selection import Selection
from textual.strip import Strip
from textual.widgets import Label, RichLog, Static


class SelectableRichLog(RichLog):
    """RichLog subclass with proper text selection support.

    The base RichLog widget lacks mouse text selection because:
    1. ``allow_select`` returns False (inherited container check)
    2. ``render_line`` doesn't call ``apply_offsets`` (needed for mouse
       coordinate mapping)
    3. No selection highlighting is rendered

    This subclass fixes all three so mouse-drag selection works.
    """

    ALLOW_SELECT = True

    @property
    def allow_select(self) -> bool:
        """Allow text selection regardless of container status."""
        return True

    def get_selection(self, selection: Selection) -> tuple[str, str] | None:
        """Extract text from the log lines under the given selection.

        Args:
            selection: Selection range information from Textual.

        Returns:
            Tuple of (extracted_text, line_ending) or None if no lines.
        """
        if not self.lines:
            return None
        # Build full text from Strip objects stored in self.lines
        full_text = "\n".join(strip.text for strip in self.lines)
        return selection.extract(full_text), "\n"

    def selection_updated(self, selection: Selection | None) -> None:
        """Clear line cache and refresh when selection changes."""
        self._line_cache.clear()
        self.refresh()

    def render_line(self, y: int) -> Strip:
        """Render a line with offset metadata and selection highlighting."""
        scroll_x, scroll_y = self.scroll_offset
        actual_y = scroll_y + y
        width = self.scrollable_content_region.width

        if actual_y >= len(self.lines):
            return Strip.blank(width, self.rich_style)

        selection = self.text_selection

        # Skip cache when selection is active (need to re-render highlights)
        if selection is None:
            key = (
                actual_y + self._start_line,
                scroll_x,
                width,
                self._widest_line_width,
            )
            if key in self._line_cache:
                line = self._line_cache[key]
                line = line.apply_offsets(scroll_x, actual_y)
                return line.apply_style(self.rich_style)

        line = self.lines[actual_y]

        # Apply selection highlighting
        if selection is not None:
            span = selection.get_span(actual_y)
            if span is not None:
                start, end = span
                line_text = line.text
                if end == -1:
                    end = len(line_text)
                sel_style = self.screen.get_component_rich_style("screen--selection")
                text_obj = Text(line_text, no_wrap=True)
                text_obj.stylize(self.rich_style)
                text_obj.stylize(sel_style, start, end)
                console = self.app.console  # pyright: ignore[reportUnknownMemberType]
                segments = list(text_obj.render(console))
                line = Strip(segments)
                line.adjust_cell_length(self._widest_line_width)

        line = line.crop_extend(scroll_x, scroll_x + width, self.rich_style)
        line = line.apply_offsets(scroll_x, actual_y)
        return line.apply_style(self.rich_style)


class CludLoopTUI(App[None]):
    """Terminal UI for clud --loop mode with streaming output and menu."""

    # Override default Textual bindings to remove ctrl+q quit shortcut.
    # Quitting is handled via double Ctrl-C or the Exit menu item.
    BINDINGS = []

    CSS = """
    Screen {
        background: $background;
        min-width: 80;
        min-height: 20;
    }

    #header {
        dock: top;
        height: 3;
        background: $accent;
        content-align: center middle;
        text-style: bold;
        color: $text;
    }

    #output {
        height: 1fr;
        border: heavy $accent;
        background: $surface;
        scrollbar-gutter: stable;
    }

    #menu_container {
        dock: bottom;
        height: auto;
        min-height: 5;
        background: $panel;
        border-top: heavy $accent;
        padding: 0 1;
    }

    #menu_items {
        text-align: center;
        padding: 1 0;
        text-style: bold;
    }

    #menu_help {
        text-align: center;
        color: $text-muted;
        padding: 0 0 1 0;
    }
    """

    MAIN_MENU = ["Options", "Exit"]
    OPTIONS_MENU = ["<- Back", "Edit UPDATE.md", "Copy All", "Halt", "Help"]

    def __init__(
        self,
        on_exit: Callable[[], None] | None = None,
        on_halt: Callable[[], None] | None = None,
        on_edit: Callable[[], None] | None = None,
        update_file: Path | None = None,
    ) -> None:
        """Initialize TUI.

        Args:
            on_exit: Callback when user exits
            on_halt: Callback when user halts loop
            on_edit: Callback when user edits UPDATE.md
            update_file: Path to UPDATE.md file for editing
        """
        super().__init__()

        self.selected_index = 0
        self.current_menu = self.MAIN_MENU
        self.on_exit_callback = on_exit
        self.on_halt_callback = on_halt
        self.on_edit_callback = on_edit
        self.update_file = update_file
        self._last_ctrl_c_time: float = 0.0

    def copy_to_clipboard(self, text: str) -> None:
        """Copy text to system clipboard using platform-specific tools.

        Textual's built-in copy_to_clipboard() uses OSC 52 terminal escape
        sequences which don't work in MSYS/Git Bash on Windows. This override
        uses platform-native clipboard commands instead.

        Args:
            text: Text to copy to clipboard.
        """
        if sys.platform == "win32":
            cmd = ["clip.exe"]
        elif sys.platform == "darwin":
            cmd = ["pbcopy"]
        else:
            cmd = ["xclip", "-selection", "clipboard"]

        try:
            subprocess.run(
                cmd,
                input=text.encode("utf-8"),
                check=True,
                capture_output=True,
                timeout=5,
            )
        except Exception:
            logging.debug("Platform clipboard command failed, falling back to OSC 52")
            super().copy_to_clipboard(text)

    def compose(self) -> ComposeResult:
        """Create child widgets."""
        yield Static("clud --loop", id="header")
        yield SelectableRichLog(auto_scroll=True, highlight=True, markup=True, id="output")

        with Container(id="menu_container"):
            yield Label(id="menu_items")
            yield Label(id="menu_help")

    def on_mount(self) -> None:
        """Called when app is mounted."""
        self.update_menu()
        self.log_message("TUI initialized. Press Ctrl-C twice to quit.")

    def log_message(self, message: str) -> None:
        """Add message to output log."""
        log = self.query_one(SelectableRichLog)
        log.write(message)

    def show_loading(self, message: str) -> None:
        """Show loading indicator with message.

        Args:
            message: Loading message to display
        """
        self.log_message("\u23f3 " + message)

    def hide_loading(self, message: str = "Done") -> None:
        """Hide loading indicator and show completion message.

        Args:
            message: Completion message to display
        """
        self.log_message("\u2713 " + message)

    def update_menu(self) -> None:
        """Update menu display based on current selection."""
        items: list[str] = []
        for i, item in enumerate(self.current_menu):
            if i == self.selected_index:
                items.append(f"[{item}]")
            else:
                items.append(f" {item} ")

        menu_label = self.query_one("#menu_items", Label)
        menu_label.update("  ".join(items))

        help_text = "\u2191\u2193/\u2190\u2192: Navigate  Enter: Select  Ctrl-C \u00d72: Exit"
        help_label = self.query_one("#menu_help", Label)
        help_label.update(help_text)

    def on_click(self, event: events.Click) -> None:
        """Handle mouse click events.

        Right-click copies any selected/highlighted text to the clipboard.
        """
        if event.button == 3:
            selected_text = self.screen.get_selected_text()
            if selected_text:
                self.copy_to_clipboard(selected_text)
                self.screen.clear_selection()
                self.notify("Copied to clipboard")

    def on_key(self, event: events.Key) -> None:
        """Handle keyboard events."""
        if event.key in ("left", "up"):
            self.selected_index = (self.selected_index - 1) % len(self.current_menu)
            self.update_menu()
        elif event.key in ("right", "down", "tab"):
            self.selected_index = (self.selected_index + 1) % len(self.current_menu)
            self.update_menu()
        elif event.key == "enter":
            self.handle_selection()
        elif event.key == "escape":
            # Escape navigates back in submenus only
            if self.current_menu == self.OPTIONS_MENU:
                self.show_main_menu()
        elif event.key == "ctrl+c":
            # If text is selected, copy it instead of triggering exit
            selected_text = self.screen.get_selected_text()
            if selected_text:
                self.copy_to_clipboard(selected_text)
                self.screen.clear_selection()
                self.notify("Copied to clipboard")
            else:
                self._handle_ctrl_c()

    def _handle_ctrl_c(self) -> None:
        """Handle Ctrl-C with double-press to exit.

        First press shows a warning. Second press within 2 seconds exits.
        If no second press within 2 seconds, the app continues normally.
        """
        now = time.monotonic()
        if now - self._last_ctrl_c_time < 2.0:
            # Second Ctrl-C within 2 seconds - exit
            if self.on_exit_callback:
                self.on_exit_callback()
            self.exit()
        else:
            # First Ctrl-C - warn user
            self._last_ctrl_c_time = now
            self.log_message("Press Ctrl-C again within 2 seconds to exit.")

    async def action_quit(self) -> None:
        """Override Textual's default quit action to use double Ctrl-C."""
        self._handle_ctrl_c()

    def action_help_quit(self) -> None:
        """Override Textual's default Ctrl-C handler that shows 'Press Ctrl+Q to quit' notification."""

    def _copy_all_output(self) -> None:
        """Copy the entire RichLog content to the clipboard."""
        log = self.query_one(SelectableRichLog)
        if not log.lines:
            self.notify("No output to copy")
            return
        full_text = "\n".join(strip.text for strip in log.lines)
        self.copy_to_clipboard(full_text)
        self.notify("All output copied to clipboard")

    def handle_selection(self) -> None:
        """Handle menu item selection."""
        selected = self.current_menu[self.selected_index]

        if selected == "Exit":
            if self.on_exit_callback:
                self.on_exit_callback()
            self.exit()
        elif selected == "Options":
            self.show_options_menu()
        elif selected == "<- Back":
            self.show_main_menu()
        elif selected == "Edit UPDATE.md":
            self.open_update_file()
        elif selected == "Copy All":
            self._copy_all_output()
        elif selected == "Halt":
            self.halt_loop()
        elif selected == "Help":
            self.show_help()

    def show_main_menu(self) -> None:
        """Switch to main menu."""
        self.current_menu = self.MAIN_MENU
        self.selected_index = 0
        self.update_menu()

    def show_options_menu(self) -> None:
        """Switch to options submenu."""
        self.current_menu = self.OPTIONS_MENU
        self.selected_index = 0
        self.update_menu()
        self.log_message("> Options menu opened")

    def open_update_file(self) -> None:
        """Open UPDATE.md in default editor."""
        if self.update_file is None:
            self.log_message("> Error: No UPDATE.md file specified")
            return

        if not self.update_file.exists():
            self.show_loading(f"Creating {self.update_file}...")
            self.update_file.parent.mkdir(parents=True, exist_ok=True)
            self.update_file.touch()
            self.hide_loading("File created")

        editor = os.environ.get("EDITOR", "notepad" if os.name == "nt" else "vi")
        self.show_loading(f"Opening {self.update_file} in {editor}...")

        try:
            # Suspend the app to allow the editor to take over the terminal
            with self.suspend():
                subprocess.run([editor, str(self.update_file)])
            self.hide_loading(f"Closed {editor}")

            # Call the on_edit callback if provided
            if self.on_edit_callback:
                self.on_edit_callback()
        except Exception as e:
            self.log_message(f"> Error opening editor: {e}")

    def halt_loop(self) -> None:
        """Halt the loop gracefully."""
        self.log_message("> Halting loop...")
        if self.on_halt_callback:
            self.on_halt_callback()
        self.exit()

    def show_help(self) -> None:
        """Display help information."""
        help_text = """
+===============================================================+
|                    CLUD LOOP TUI - HELP                        |
+===============================================================+
|                                                                |
|  KEYBOARD SHORTCUTS:                                           |
|    Up / Down / Left / Right    Navigate menu items             |
|    Tab              Next menu item                             |
|    Enter            Select highlighted menu item               |
|    Esc              Back to previous menu                      |
|    Ctrl-C           Copy selected text / x2 to quit            |
|                                                                |
|  MENU OPTIONS:                                                 |
|    Options          Open options submenu                       |
|    Exit             Quit clud --loop                           |
|                                                                |
|  OPTIONS SUBMENU:                                              |
|    <- Back          Return to main menu                        |
|    Edit UPDATE.md   Open UPDATE.md in default editor           |
|    Copy All         Copy all output to clipboard               |
|    Halt             Stop loop gracefully                       |
|    Help             Show this help screen                      |
|                                                                |
|  REQUIREMENTS:                                                 |
|    Minimum terminal size: 80x20                                |
|    UTF-8 encoding support recommended                          |
|    ANSI color support (optional)                               |
|                                                                |
|  Press any key to close this help screen                       |
+===============================================================+
"""
        self.log_message(help_text)

    @work(exclusive=False)
    async def stream_output(self, stream: AsyncIterator[str]) -> None:
        """Stream output from async iterator to log.

        Args:
            stream: Async iterator yielding output lines
        """
        log = self.query_one(SelectableRichLog)

        try:
            async for line in stream:
                log.write(line.rstrip())
        except Exception as e:
            log.write(f"> Error streaming output: {e}")

    def on_resize(self, event: events.Resize) -> None:
        """Handle terminal resize events.

        Args:
            event: Resize event
        """
        # Warn if terminal too small
        if event.size.width < 80 or event.size.height < 20:
            self.notify(
                "Terminal too small! Minimum 80x20 recommended.",
                severity="warning",
            )
