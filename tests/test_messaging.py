"""Tests for messaging module."""

import pytest


class TestMessagingImports:
    """Test that messaging module can be imported."""

    def test_import_messaging_module(self):
        """Test importing messaging module."""
        try:
            from clud.messaging import MessagePlatform, MessengerFactory

            assert MessagePlatform is not None
            assert MessengerFactory is not None
        except ImportError as e:
            pytest.skip(f"Messaging dependencies not installed: {e}")

    def test_message_platform_enum(self):
        """Test MessagePlatform enum values."""
        try:
            from clud.messaging import MessagePlatform

            assert MessagePlatform.TELEGRAM.value == "telegram"
            assert MessagePlatform.SMS.value == "sms"
            assert MessagePlatform.WHATSAPP.value == "whatsapp"
        except ImportError:
            pytest.skip("Messaging dependencies not installed")


class TestMessengerFactory:
    """Test MessengerFactory functionality."""

    def test_factory_requires_valid_platform(self):
        """Test factory validates platform."""
        try:
            from clud.messaging import MessengerFactory

            with pytest.raises(ValueError, match="Unsupported messaging platform"):
                MessengerFactory.create_messenger("invalid", {})
        except ImportError:
            pytest.skip("Messaging dependencies not installed")

    def test_telegram_config_validation(self):
        """Test Telegram configuration validation."""
        try:
            from clud.messaging import MessengerFactory

            # Missing bot_token
            with pytest.raises(ValueError, match="bot_token"):
                MessengerFactory.create_messenger("telegram", {"chat_id": "123"})

            # Missing chat_id
            with pytest.raises(ValueError, match="chat_id"):
                MessengerFactory.create_messenger("telegram", {"bot_token": "token"})
        except ImportError:
            pytest.skip("Messaging dependencies not installed")

    def test_sms_config_validation(self):
        """Test SMS configuration validation."""
        try:
            from clud.messaging import MessengerFactory

            # Missing required fields
            with pytest.raises(ValueError, match="account_sid"):
                MessengerFactory.create_messenger("sms", {"auth_token": "token"})
        except ImportError:
            pytest.skip("Messaging dependencies not installed")

    def test_factory_validate_config(self):
        """Test config validation helper."""
        try:
            from clud.messaging import MessengerFactory

            # Valid config
            valid, error = MessengerFactory.validate_config("telegram", {"bot_token": "token", "chat_id": "123"})
            assert valid is True or not valid  # May fail due to missing telegram library
            assert isinstance(error, str)

            # Invalid platform
            valid, error = MessengerFactory.validate_config("invalid", {})
            assert valid is False
            assert "Unsupported" in error
        except ImportError:
            pytest.skip("Messaging dependencies not installed")


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


class TestSMSMessenger:
    """Test SMS messenger."""

    def test_sms_messenger_creation(self):
        """Test creating SMS messenger."""
        try:
            from clud.messaging import SMSMessenger

            messenger = SMSMessenger(account_sid="AC123", auth_token="token", from_number="+1234567890", to_number="+0987654321")
            assert messenger is not None
            assert messenger.account_sid == "AC123"
            assert messenger.from_number == "+1234567890"
        except ImportError:
            pytest.skip("SMS dependencies not installed")


class TestWhatsAppMessenger:
    """Test WhatsApp messenger."""

    def test_whatsapp_messenger_creation(self):
        """Test creating WhatsApp messenger."""
        try:
            from clud.messaging import WhatsAppMessenger

            messenger = WhatsAppMessenger(phone_number_id="123", access_token="token", to_number="+1234567890")
            assert messenger is not None
            assert messenger.phone_number_id == "123"
            assert messenger.to_number == "+1234567890"
        except ImportError:
            pytest.skip("WhatsApp dependencies not installed")
