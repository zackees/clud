"""Test the Telegram status endpoint behavior."""

import unittest

from clud.agent_cli import save_telegram_credentials
from clud.webui.telegram_api import TelegramAPIHandler


class TestTelegramStatusEndpoint(unittest.TestCase):
    """Test the status endpoint logic."""

    def test_status_with_invalid_token(self) -> None:
        """Test status endpoint with invalid test token."""
        # Save a test token (invalid for Telegram)
        test_token = "test-invalid-token-12345"
        save_telegram_credentials(test_token, "")

        # Create handler
        handler = TelegramAPIHandler()

        # Check credentials are saved
        bot_token, chat_id = handler.get_credentials()
        print("\n=== Stored Credentials ===")
        print(f"Bot Token: {bot_token[:20] + '...' if bot_token else 'NOT SET'}")
        print(f"Chat ID: {chat_id if chat_id else 'NOT SET'}")

        self.assertEqual(bot_token, test_token, "Token should be saved")

        # Test connection (should fail for invalid token)
        bot_info = handler.test_bot_connection_sync(bot_token)  # type: ignore[arg-type]

        print("\n=== Connection Test Result ===")
        print(f"Bot Info: {bot_info}")
        print(f"Connection Success: {bot_info is not None}")

        # For invalid token, should return None
        self.assertIsNone(bot_info, "Invalid token should not return bot info")

        print("\n✓ Invalid token correctly returns None")

    def test_status_endpoint_logic(self) -> None:
        """Test the logic that the /api/telegram/status endpoint uses."""
        # Save invalid token
        test_token = "test-token-123"
        save_telegram_credentials(test_token, "12345")

        handler = TelegramAPIHandler()
        bot_token, chat_id = handler.get_credentials()

        print("\n=== Status Endpoint Simulation ===")
        print("Step 1: Get credentials from keyring")
        print(f"  bot_token: {bot_token[:20] + '...' if bot_token else 'None'}")
        print(f"  chat_id: {chat_id}")

        # This simulates what the status endpoint does
        if bot_token:
            print("\nStep 2: Test bot connection")
            bot_info = handler.test_bot_connection_sync(bot_token)
            print(f"  bot_info: {bot_info}")

            print("\nStep 3: Build response")
            response = {
                "connected": bot_info is not None,
                "credentials_saved": True,
                "bot_info": bot_info,
                "chat_id": chat_id,
            }
            print(f"  Response: {response}")

            # Verify response structure
            self.assertTrue(response["credentials_saved"], "Should show credentials saved")
            self.assertFalse(response["connected"], "Should NOT be connected with invalid token")
            self.assertIsNone(response["bot_info"], "Should have no bot info with invalid token")

            print("\n✓ Status endpoint would return credentials_saved=True but connected=False")
        else:
            self.fail("No token found - save failed")


if __name__ == "__main__":
    unittest.main()
