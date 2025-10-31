#!/usr/bin/env python3
"""Clear Telegram credentials from Windows Credential Manager directly."""

import sys
from pathlib import Path

# Add src to path to import modules
sys.path.insert(0, str(Path(__file__).parent / "src"))

try:
    import keyring
except ImportError:
    print("ERROR: keyring library not available")
    sys.exit(1)

TELEGRAM_SERVICE = "clud-telegram"
BOT_TOKEN_KEY = "bot-token"
CHAT_ID_KEY = "chat-id"


def clear_telegram_credentials() -> None:
    """Clear Telegram credentials from Windows Credential Manager."""
    print("Clearing Telegram credentials from Windows Credential Manager...")
    print()

    # Show current credentials
    bot_token = keyring.get_password(TELEGRAM_SERVICE, BOT_TOKEN_KEY)
    chat_id = keyring.get_password(TELEGRAM_SERVICE, CHAT_ID_KEY)

    if bot_token:
        masked = bot_token[:10] + "..." + bot_token[-10:] if len(bot_token) > 20 else bot_token[:5] + "..."
        print(f"Current bot token: {masked}")
    else:
        print("Current bot token: None")

    if chat_id:
        print(f"Current chat ID: {chat_id}")
    else:
        print("Current chat ID: None")

    print()

    # Clear credentials
    try:
        # Delete credentials (set to empty string works for Windows Credential Manager)
        keyring.set_password(TELEGRAM_SERVICE, BOT_TOKEN_KEY, "")
        keyring.set_password(TELEGRAM_SERVICE, CHAT_ID_KEY, "")

        print("✓ Telegram credentials cleared successfully!")
        print()
        print("Next steps:")
        print("1. Refresh the Web UI in your browser")
        print("2. Go to Settings > Telegram Integration")
        print("3. The status should show 'Disconnected'")
        print("4. Get a valid bot token from @BotFather on Telegram:")
        print("   - Open https://t.me/BotFather")
        print("   - Send: /newbot")
        print("   - Follow the prompts to create your bot")
        print("   - Copy the token (format: 1234567890:ABC...)")
        print("5. Paste the token in the Web UI and click 'Save Credentials'")

    except Exception as e:
        print(f"ERROR: Failed to clear credentials: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)


if __name__ == "__main__":
    clear_telegram_credentials()
