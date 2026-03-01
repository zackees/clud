"""Integration between loop logic and TUI."""

from pathlib import Path
from typing import TYPE_CHECKING

from ..util import handle_keyboard_interrupt
from .loop_worker import LoopWorkerApp

if TYPE_CHECKING:
    from ..agent_args import Args


def run_loop_with_tui(args: "Args", claude_path: str, loop_count: int) -> int:
    """Run loop mode with TUI.

    This function wraps the standard loop execution with a TUI that displays
    streaming output and provides interactive menu options.

    Args:
        args: Command-line arguments
        claude_path: Path to Claude executable
        loop_count: Number of iterations to run

    Returns:
        Exit code (0 for success)
    """
    # Handle existing .loop/ session BEFORE launching the TUI,
    # since _handle_existing_loop uses input() which requires a real terminal.
    from ..agent.task_manager import _handle_existing_loop

    loop_dir = Path(".loop")
    should_continue, start_iteration = _handle_existing_loop(loop_dir)
    if not should_continue:
        return 2

    # Set up UPDATE.md file path (.loop/UPDATE.md)
    update_file = loop_dir / "UPDATE.md"

    # Create and run the TUI app with loop worker
    app = LoopWorkerApp(
        args=args,
        claude_path=claude_path,
        loop_count=loop_count,
        update_file=update_file,
        start_iteration=start_iteration,
    )

    # Run the TUI app (blocking). Catch stray KeyboardInterrupt that
    # bypasses Textual's event loop (e.g. on MSYS/mintty) to ensure
    # the active subprocess is killed promptly.
    try:
        app.run()
    except KeyboardInterrupt as e:
        app._kill_active_subprocess()
        handle_keyboard_interrupt(e)
        return 130  # Worker thread: suppressed

    # Return the exit code from the loop execution
    return app._exit_code
