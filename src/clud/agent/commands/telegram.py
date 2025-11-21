"""Telegram command handler for clud agent."""

import os
import sys

from clud.agent.config import load_telegram_credentials, save_telegram_credentials


def handle_telegram_command(token: str | None = None) -> int:
    """Handle the --telegram/-tg command by launching Telegram integration server via daemon.

    Automatically starts the daemon-based Telegram server if credentials are available.
    Falls back to landing page if no credentials found.

    Args:
        token: Optional bot token to save

    Returns:
        Exit code
    """
    try:
        # Import telegram_server handler
        from clud.agent.commands.telegram_server import handle_telegram_server_command

        # Save token if provided
        if token:
            print("Saving Telegram bot token...")
            try:
                save_telegram_credentials(token, "")
                print("✓ Token saved successfully\n")
            except Exception as e:
                print(f"Warning: Could not save token: {e}\n", file=sys.stderr)

        # Load credentials from environment or saved config
        saved_token, _ = load_telegram_credentials()
        env_token = os.environ.get("TELEGRAM_BOT_TOKEN")

        # Prioritize env vars, fall back to saved
        bot_token = env_token or saved_token or token

        # If we have a bot token, launch Telegram server via daemon
        if bot_token:
            print("✅ Telegram bot token found")
            print(f"Bot Token: {bot_token[:20]}...")
            print()
            # Launch the full Telegram integration server via daemon
            return handle_telegram_server_command(port=None, config_path=None)

        # Otherwise, launch landing page mode
        print("⚠️  No Telegram bot token found")
        print("Please provide a bot token:")
        print("  1. Set TELEGRAM_BOT_TOKEN environment variable, OR")
        print("  2. Run: clud --telegram YOUR_BOT_TOKEN")
        print()
        print("To get a bot token, message @BotFather on Telegram")
        print()
        print("Launching landing page...")
        print()

        from clud.webapp.server import run_server

        return run_server()

    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        return 1
