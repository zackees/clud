"""Web UI command handler for clud agent."""

import sys


def handle_webui_command(port: int | None = None) -> int:
    """Handle the --webui command by launching Web UI server."""
    from clud.webui import WebUI

    try:
        print("Starting Claude Code Web UI...")
        return WebUI.run_server(port)
    except Exception as e:
        print(f"Error running Web UI: {e}", file=sys.stderr)
        return 1
