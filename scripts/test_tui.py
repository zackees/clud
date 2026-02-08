"""Test script for TUI with all Phase 2 features."""

from pathlib import Path

from clud.loop_tui import LoopTUI, TUIConfig


def on_exit() -> None:
    """Exit callback."""
    print("Exit callback triggered")


def on_halt() -> None:
    """Halt callback."""
    print("Halt callback triggered")


def on_edit() -> None:
    """Edit callback."""
    print("Edit callback triggered - UPDATE.md was edited")


if __name__ == "__main__":
    # Create a test UPDATE.md path
    update_file = Path(".loop/UPDATE.md")

    config = TUIConfig(
        on_exit=on_exit,
        on_halt=on_halt,
        on_edit=on_edit,
        update_file=update_file,
    )
    LoopTUI.run(config)
