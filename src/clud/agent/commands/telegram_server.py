"""Telegram server command handler for clud agent."""

import sys


def handle_telegram_server_command(port: int | None = None, config_path: str | None = None) -> int:
    """Handle the --telegram-server command by ensuring telegram service via daemon.

    Args:
        port: Optional port for web interface (default: 8889)
        config_path: Optional path to configuration file

    Returns:
        Exit code
    """
    try:
        import json
        import urllib.request
        import webbrowser

        from clud.service import ensure_telegram_running
        from clud.service.server import DAEMON_HOST, DAEMON_PORT

        print("Starting Telegram Integration Server via daemon...")
        print()

        # Ensure telegram service is running via daemon
        if not ensure_telegram_running(config_path=config_path, port=port):
            print("ERROR: Failed to start telegram service", file=sys.stderr)
            print("Check logs for details:", file=sys.stderr)
            print("  - Daemon logs: ~/.config/clud/daemon.log (if logging enabled)", file=sys.stderr)
            print("  - Telegram config: Use --telegram-config to specify config file", file=sys.stderr)
            print("  - Bot token: Set TELEGRAM_BOT_TOKEN environment variable", file=sys.stderr)
            return 1

        # Get status to display info
        status_url = f"http://{DAEMON_HOST}:{DAEMON_PORT}/telegram/status"
        try:
            with urllib.request.urlopen(status_url, timeout=2.0) as response:
                status = json.loads(response.read().decode("utf-8"))

                print("✓ Telegram service is running")
                print()
                print("Configuration:")
                print(f"  Bot Token: {'✓ Configured' if status.get('bot_configured') else '✗ Missing'}")
                print(f"  Web URL: http://{status.get('host', '127.0.0.1')}:{status.get('port', 8889)}")
                print(f"  Daemon Port: {DAEMON_PORT}")
                print()

                # Open browser
                web_url = f"http://{status.get('host', '127.0.0.1')}:{status.get('port', 8889)}"
                print(f"Opening browser to {web_url}...")
                webbrowser.open(web_url)
                print()
                print("Service is running in background via daemon (port 7565)")
                print("Use 'clud --telegram-server' again to check status")
                print("To stop: Contact daemon or restart system")

        except Exception as e:
            print(f"Warning: Could not retrieve status: {e}", file=sys.stderr)
            print("Service may be starting... check http://127.0.0.1:8889", file=sys.stderr)

        return 0

    except ImportError as e:
        print(f"Error: Missing required dependency: {e}", file=sys.stderr)
        print("Install with: pip install python-telegram-bot pyyaml", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        import traceback

        traceback.print_exc()
        return 1
