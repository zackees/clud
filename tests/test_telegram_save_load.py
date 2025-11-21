"""Test Telegram credential save and load."""

import unittest

from clud.agent import load_telegram_credentials, save_telegram_credentials
from clud.secrets import get_credential_store


class TestTelegramSaveLoad(unittest.TestCase):
    """Test saving and loading Telegram credentials."""

    def test_save_and_load_bot_token(self) -> None:
        """Test that we can save and load a bot token."""
        # Verify credential store is available
        store = get_credential_store()
        self.assertIsNotNone(store, "Credential store must be available")

        # Test token (proper Telegram token format: bot_id:secret)
        test_token = "123456789:ABCdefGHI-jklMNOpqr123456"

        print("\n=== Testing Save/Load Cycle ===")
        print(f"1. Saving test token: {test_token[:15]}...")

        try:
            # Save the token
            save_telegram_credentials(test_token, "")
            print("   ✓ Token saved successfully")
        except Exception as e:
            print(f"   ✗ Failed to save token: {e}")
            self.fail(f"Failed to save token: {e}")

        # Load it back
        print("2. Loading token back...")
        loaded_token, loaded_chat_id = load_telegram_credentials()

        print(f"   Loaded Token: {loaded_token[:15] + '...' if loaded_token else 'None'}")
        print(f"   Loaded Chat ID: {loaded_chat_id if loaded_chat_id else 'None'}")

        # Verify
        self.assertIsNotNone(loaded_token, "Token should not be None")
        self.assertEqual(loaded_token, test_token, "Loaded token should match saved token")
        print("   ✓ Token matches!")

        print("\n3. Verifying direct keyring access...")
        if store:
            direct_token = store.get_password("clud-telegram", "bot-token")
            print(f"   Direct Token: {direct_token[:15] + '...' if direct_token else 'None'}")
            self.assertEqual(direct_token, test_token, "Direct keyring access should return same token")
            print("   ✓ Direct access works!")

        print("\n=== Test Complete ===\n")

    def test_current_stored_token(self) -> None:
        """Test what's currently stored in the credential store."""
        print("\n=== Current Stored Credentials ===")

        store = get_credential_store()
        if store:
            bot_token = store.get_password("clud-telegram", "bot-token")
            chat_id = store.get_password("clud-telegram", "chat-id")

            print("Service: clud-telegram")
            print(f"Bot Token: {bot_token[:20] + '...' if bot_token else 'NOT SET'}")
            print(f"Chat ID: {chat_id if chat_id else 'NOT SET'}")

            if bot_token:
                print(f"\n✓ Bot token IS stored (length: {len(bot_token)})")
            else:
                print("\n✗ Bot token is NOT stored")

        print("===================================\n")


if __name__ == "__main__":
    unittest.main()
