"""Test that Web UI and CLI share the same Telegram credential storage."""

import unittest

from clud.agent import load_telegram_credentials, save_telegram_credentials
from clud.secrets import get_credential_store
from clud.webui.telegram_api import TelegramAPIHandler


class TestTelegramCredentialSync(unittest.TestCase):
    """Test that Web UI and CLI use the same credential storage location."""

    def test_credential_storage_keys_match(self) -> None:
        """Test that Web UI and CLI use the same service and key names."""
        from clud.webui import telegram_api

        # Check service names match
        self.assertEqual(
            telegram_api.TELEGRAM_SERVICE,
            "clud-telegram",
            "Web UI must use same service name as CLI (clud-telegram)",
        )

        # Check key names match
        self.assertEqual(telegram_api.BOT_TOKEN_KEY, "bot-token", "Web UI must use same bot token key as CLI (bot-token)")

        self.assertEqual(telegram_api.CHAT_ID_KEY, "chat-id", "Web UI must use same chat ID key as CLI (chat-id)")

        print("\n✓ Web UI and CLI use matching credential storage keys")

    def test_webui_save_cli_load(self) -> None:
        """Test that credentials saved via Web UI can be loaded by CLI."""
        test_token = "webui-test-token-12345"
        test_chat_id = "123456789"

        # Save via Web UI handler
        handler = TelegramAPIHandler()
        success = handler.save_credentials(test_token, test_chat_id)
        self.assertTrue(success, "Web UI should save credentials successfully")

        # Load via CLI function
        loaded_token, loaded_chat_id = load_telegram_credentials()

        self.assertEqual(loaded_token, test_token, "CLI should load token saved by Web UI")
        self.assertEqual(loaded_chat_id, test_chat_id, "CLI should load chat ID saved by Web UI")

        print("\n✓ Credentials saved by Web UI can be loaded by CLI")

    def test_cli_save_webui_load(self) -> None:
        """Test that credentials saved via CLI can be loaded by Web UI."""
        test_token = "cli-test-token-67890"
        test_chat_id = "987654321"

        # Save via CLI function
        save_telegram_credentials(test_token, test_chat_id)

        # Load via Web UI handler
        handler = TelegramAPIHandler()
        loaded_token, loaded_chat_id = handler.get_credentials()

        self.assertEqual(loaded_token, test_token, "Web UI should load token saved by CLI")
        self.assertEqual(loaded_chat_id, test_chat_id, "Web UI should load chat ID saved by CLI")

        print("\n✓ Credentials saved by CLI can be loaded by Web UI")

    def test_direct_keyring_access(self) -> None:
        """Test direct keyring access shows same location."""
        test_token = "direct-test-token-11111"

        # Save via CLI
        save_telegram_credentials(test_token, "")

        # Load directly from keyring
        store = get_credential_store()
        self.assertIsNotNone(store)

        if store:
            direct_token = store.get_password("clud-telegram", "bot-token")
            self.assertEqual(direct_token, test_token, "Direct keyring access should match saved token")

        print("\n✓ Direct keyring access confirms correct storage location")


if __name__ == "__main__":
    unittest.main()
