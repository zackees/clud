#!/usr/bin/env python3
"""Clear Telegram credentials from keyring."""

import sys
from pathlib import Path

# Add src to path to import modules
sys.path.insert(0, str(Path(__file__).parent / "src"))

from clud.secrets import get_credential_store

TELEGRAM_SERVICE = "clud-telegram"
BOT_TOKEN_KEY = "bot-token"
CHAT_ID_KEY = "chat-id"


def clear_telegram_credentials() -> None:
    """Clear Telegram credentials from keyring."""
    credential_store = get_credential_store()

    if credential_store is None:
        print("ERROR: No credential store available")
        sys.exit(1)

    try:
        # Clear credentials by setting empty strings
        credential_store.set_password(TELEGRAM_SERVICE, BOT_TOKEN_KEY, "")
        credential_store.set_password(TELEGRAM_SERVICE, CHAT_ID_KEY, "")
        print("✓ Telegram credentials cleared successfully")
        print("\nNow you can:")
        print("1. Restart the Web UI")
        print("2. Go to Settings > Telegram Integration")
        print("3. Enter a valid bot token from @BotFather")
    except Exception as e:
        print(f"ERROR: Failed to clear credentials: {e}")
        sys.exit(1)


if __name__ == "__main__":
    clear_telegram_credentials()
