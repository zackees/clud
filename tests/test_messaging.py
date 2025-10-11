"""Tests for Telegram messaging module."""

import pytest


class TestMessagingImports:
    """Test that messaging module can be imported."""

    def test_import_messaging_module(self):
        """Test importing messaging module."""
        try:
            from clud.messaging import TelegramMessenger

            assert TelegramMessenger is not None
        except ImportError as e:
            pytest.skip(f"Telegram dependencies not installed: {e}")


class TestTelegramMessenger:
    """Test Telegram messenger."""

    def test_telegram_messenger_creation(self):
        """Test creating Telegram messenger."""
        try:
            from clud.messaging import TelegramMessenger

            messenger = TelegramMessenger(bot_token="test_token", chat_id="123456789")
            assert messenger is not None
            assert messenger.bot_token == "test_token"
            assert messenger.chat_id == "123456789"
        except ImportError:
            pytest.skip("Telegram dependencies not installed")

    def test_telegram_config_validation(self):
        """Test Telegram configuration validation."""
        try:
            from clud.messaging.factory import validate_telegram_config

            # Valid config
            valid, error = validate_telegram_config("token", "123")
            assert valid is True
            assert error == ""

            # Missing token
            valid, error = validate_telegram_config("", "123")
            assert valid is False
            assert "bot_token" in error

            # Missing chat_id
            valid, error = validate_telegram_config("token", "")
            assert valid is False
            assert "chat_id" in error
        except ImportError:
            pytest.skip("Telegram dependencies not installed")
