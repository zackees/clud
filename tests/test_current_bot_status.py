"""Test the current bot token status."""

import unittest

from clud.agent_cli import load_telegram_credentials
from clud.webui.telegram_api import TelegramAPIHandler


class TestCurrentBotStatus(unittest.TestCase):
    """Test what's currently happening with the saved token."""

    def test_current_token_validity(self) -> None:
        """Test if the currently saved token is valid."""
        # Load from keyring
        bot_token, chat_id = load_telegram_credentials()

        print("\n=== Current Credentials ===")
        print(f"Bot Token: {bot_token[:30] + '...' if bot_token and len(bot_token) > 30 else bot_token}")
        print(f"Token Length: {len(bot_token) if bot_token else 0}")
        print(f"Chat ID: {chat_id}")
        print(f"Token starts with valid format: {bot_token.startswith('test-') if bot_token else 'N/A'}")

        if not bot_token:
            print("\n❌ NO TOKEN FOUND - You need to save your bot token!")
            return

        # Check if it's a test token
        if bot_token.startswith("test-") or bot_token.startswith("webui-test"):
            print("\n⚠️  This is a TEST TOKEN, not a real Telegram bot token!")
            print("Real Telegram bot tokens look like: 1234567890:ABCdefGHIjklMNOpqrsTUVwxyz")
            print("\nYou need to:")
            print("  1. Get your real bot token from @BotFather on Telegram")
            print("  2. Restart the Web UI: clud --webui")
            print("  3. Go to Settings → Telegram")
            print("  4. Enter your REAL bot token and click Save Credentials")
            return

        # Try to test connection
        print("\n=== Testing Connection ===")
        handler = TelegramAPIHandler()
        bot_info = handler.test_bot_connection_sync(bot_token)

        if bot_info:
            print("✅ CONNECTION SUCCESSFUL!")
            print(f"Bot Username: @{bot_info['username']}")
            print(f"Bot Name: {bot_info['first_name']}")
            print(f"Bot ID: {bot_info['id']}")
            print("\n✨ Launch in Telegram button should be ENABLED")
        else:
            print("❌ CONNECTION FAILED")
            print("\nPossible reasons:")
            print("  1. Invalid bot token format")
            print("  2. Bot token has been revoked by @BotFather")
            print("  3. Network connectivity issues")
            print("  4. python-telegram-bot library not installed")
            print("\n💡 Try:")
            print("  1. Verify token with @BotFather on Telegram")
            print("  2. Check your internet connection")
            print("  3. Run: uv pip install python-telegram-bot")


if __name__ == "__main__":
    unittest.main()
