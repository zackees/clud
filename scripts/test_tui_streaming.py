"""Test script for TUI streaming output."""

import asyncio
from collections.abc import AsyncIterator
from pathlib import Path

from clud.loop_tui import LoopTUI, TUIConfig


async def mock_stream() -> AsyncIterator[str]:
    """Mock streaming process that yields lines over time."""
    messages = [
        "Starting iteration #1...",
        "Reading .loop/LOOP.md...",
        "Processing task...",
        "Analyzing codebase...",
        "Implementing feature...",
        "Writing code...",
        "Running tests...",
        "All tests passed!",
        "Iteration complete.",
        "",
        "Waiting for UPDATE.md changes...",
    ]

    for i, message in enumerate(messages):
        yield f"[{i+1:2d}] {message}"
        await asyncio.sleep(0.3)  # Simulate streaming delay


def on_exit() -> None:
    """Exit callback."""
    print("Exit callback triggered")


def on_halt() -> None:
    """Halt callback."""
    print("Halt callback triggered")


def on_edit() -> None:
    """Edit callback."""
    print("Edit callback triggered - UPDATE.md was edited")


async def main() -> None:
    """Main function to run TUI with streaming."""
    update_file = Path(".loop/UPDATE.md")

    config = TUIConfig(
        on_exit=on_exit,
        on_halt=on_halt,
        on_edit=on_edit,
        update_file=update_file,
    )

    # Note: This is a simplified test. In practice, the TUI app.run() is blocking
    # and we would integrate streaming differently. This shows the API.
    print("Testing TUI with streaming capability...")
    print("To test streaming in the actual TUI, use the stream_output() method")
    print("after mounting the app.")

    # Run the TUI normally
    LoopTUI.run(config)


if __name__ == "__main__":
    asyncio.run(main())
