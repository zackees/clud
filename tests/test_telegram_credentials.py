"""Unit tests for Telegram credential management."""

import unittest

from clud.agent_cli import load_telegram_credentials
from clud.secrets import get_credential_store


class TestTelegramCredentials(unittest.TestCase):
    """Test Telegram credential loading."""

    def test_credential_store_available(self) -> None:
        """Test that credential store is available."""
        store = get_credential_store()
        self.assertIsNotNone(store, "Credential store should be available")

    def test_load_telegram_bot_token(self) -> None:
        """Test loading telegram bot token from credential store."""
        bot_token, chat_id = load_telegram_credentials()

        print("\n=== Telegram Credentials Test ===")
        print(f"Bot Token: {bot_token[:20] + '...' if bot_token else 'NOT SET'}")
        print(f"Chat ID: {chat_id if chat_id else 'NOT SET'}")
        print("================================\n")

        # This test will pass even if token is not set, but will show the results
        # If token is set, verify it's not empty
        if bot_token:
            self.assertIsInstance(bot_token, str)
            self.assertGreater(len(bot_token), 0)
            print("✓ Bot token is set and non-empty")
        else:
            print("⚠ Bot token is NOT set")
            print("\nTo set the bot token, run:")
            print("  clud --telegram YOUR_BOT_TOKEN")
            print("Or:")
            print("  export TELEGRAM_BOT_TOKEN=YOUR_BOT_TOKEN")

    def test_load_telegram_credentials_directly(self) -> None:
        """Test loading credentials directly from keyring."""
        store = get_credential_store()
        self.assertIsNotNone(store)

        if store:
            # Try to load bot token directly
            bot_token = store.get_password("clud-telegram", "bot-token")
            chat_id = store.get_password("clud-telegram", "chat-id")

            print("\n=== Direct Keyring Access Test ===")
            print("Service: clud-telegram")
            print("Bot Token Username: bot-token")
            print(f"Bot Token: {bot_token[:20] + '...' if bot_token else 'NOT SET'}")
            print("Chat ID Username: chat-id")
            print(f"Chat ID: {chat_id if chat_id else 'NOT SET'}")
            print("===================================\n")

            if bot_token:
                print("✓ Bot token found in credential store")
            else:
                print("✗ Bot token NOT found in credential store")


if __name__ == "__main__":
    unittest.main()
