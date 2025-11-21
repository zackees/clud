"""Kanban command handler for clud agent."""

import sys


def handle_kanban_command() -> int:
    """Handle the --kanban command by setting up and running vibe-kanban."""
    from clud.kanban_manager import setup_and_run_kanban

    try:
        return setup_and_run_kanban()
    except Exception as e:
        print(f"Error running kanban: {e}", file=sys.stderr)
        return 1
