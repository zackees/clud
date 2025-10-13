"""Tests for Telegram messaging module."""

import unittest


class TestMessagingImports(unittest.TestCase):
    """Test that messaging module can be imported."""

    def test_import_messaging_module(self) -> None:
        """Test importing messaging module."""
        try:
            from clud.messaging import TelegramMessenger

            self.assertIsNotNone(TelegramMessenger)
        except ImportError as e:
            self.skipTest(f"Telegram dependencies not installed: {e}")


class TestTelegramMessenger(unittest.TestCase):
    """Test Telegram messenger."""

    def test_telegram_messenger_creation(self) -> None:
        """Test creating Telegram messenger."""
        try:
            from clud.messaging import TelegramMessenger

            messenger = TelegramMessenger(bot_token="test_token", chat_id="123456789")
            self.assertIsNotNone(messenger)
            self.assertEqual(messenger.bot_token, "test_token")
            self.assertEqual(messenger.chat_id, "123456789")
        except ImportError:
            self.skipTest("Telegram dependencies not installed")

    def test_telegram_config_validation(self) -> None:
        """Test Telegram configuration validation."""
        try:
            from clud.messaging.factory import validate_telegram_config

            # Valid config
            valid, error = validate_telegram_config("token", "123")
            self.assertTrue(valid)
            self.assertEqual(error, "")

            # Missing token
            valid, error = validate_telegram_config("", "123")
            self.assertFalse(valid)
            self.assertIn("bot_token", error)

            # Missing chat_id
            valid, error = validate_telegram_config("token", "")
            self.assertFalse(valid)
            self.assertIn("chat_id", error)
        except ImportError:
            self.skipTest("Telegram dependencies not installed")


if __name__ == "__main__":
    unittest.main()
