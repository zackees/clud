"""Unit tests for messaging module."""

import pytest
from unittest.mock import AsyncMock, Mock, patch

from clud.messaging.factory import create_client, validate_contact_format
from clud.messaging.config import load_messaging_config


class TestContactValidation:
    """Test contact format validation."""

    def test_validate_telegram_username(self):
        """Test Telegram @username format."""
        valid, channel = validate_contact_format("@username")
        assert valid is True
        assert channel == "telegram"

    def test_validate_telegram_chat_id(self):
        """Test Telegram numeric chat_id format."""
        valid, channel = validate_contact_format("123456789")
        assert valid is True
        assert channel == "telegram"

    def test_validate_telegram_prefixed(self):
        """Test telegram: prefixed format."""
        valid, channel = validate_contact_format("telegram:@username")
        assert valid is True
        assert channel == "telegram"

        valid, channel = validate_contact_format("telegram:123456789")
        assert valid is True
        assert channel == "telegram"

    def test_validate_sms(self):
        """Test SMS phone number format."""
        valid, channel = validate_contact_format("+1234567890")
        assert valid is True
        assert channel == "sms"

    def test_validate_whatsapp(self):
        """Test WhatsApp format."""
        valid, channel = validate_contact_format("whatsapp:+1234567890")
        assert valid is True
        assert channel == "whatsapp"

    def test_validate_invalid(self):
        """Test invalid contact format."""
        valid, channel = validate_contact_format("invalid")
        assert valid is False
        assert channel == "unknown"


class TestMessagingFactory:
    """Test messaging client factory."""

    def test_create_telegram_client_username(self):
        """Test creating Telegram client with @username."""
        config = {"telegram_token": "fake_token"}
        client = create_client("@username", config)
        assert client is not None
        assert hasattr(client, "send_message")

    def test_create_telegram_client_chat_id(self):
        """Test creating Telegram client with chat_id."""
        config = {"telegram_token": "fake_token"}
        client = create_client("123456789", config)
        assert client is not None

    def test_create_sms_client(self):
        """Test creating SMS client."""
        config = {"twilio_sid": "ACfake", "twilio_token": "fake_token", "twilio_number": "+15555555555"}
        client = create_client("+1234567890", config)
        assert client is not None

    def test_create_whatsapp_client(self):
        """Test creating WhatsApp client."""
        config = {"twilio_sid": "ACfake", "twilio_token": "fake_token", "twilio_number": "+15555555555"}
        client = create_client("whatsapp:+1234567890", config)
        assert client is not None

    def test_create_client_missing_telegram_config(self):
        """Test creating Telegram client without config."""
        config = {}
        with pytest.raises(ValueError, match="Telegram token not configured"):
            create_client("@username", config)

    def test_create_client_missing_twilio_config(self):
        """Test creating SMS client without config."""
        config = {}
        with pytest.raises(ValueError, match="Twilio configuration missing"):
            create_client("+1234567890", config)

    def test_create_client_invalid_format(self):
        """Test creating client with invalid contact format."""
        config = {"telegram_token": "fake_token"}
        with pytest.raises(ValueError, match="Invalid contact format"):
            create_client("invalid-format", config)


class TestMessagingConfig:
    """Test messaging configuration management."""

    def test_load_config_from_env(self, monkeypatch):
        """Test loading config from environment variables."""
        monkeypatch.setenv("TELEGRAM_BOT_TOKEN", "test_telegram_token")
        monkeypatch.setenv("TWILIO_ACCOUNT_SID", "test_sid")
        monkeypatch.setenv("TWILIO_AUTH_TOKEN", "test_token")
        monkeypatch.setenv("TWILIO_FROM_NUMBER", "+15555555555")

        config = load_messaging_config()

        assert config["telegram_token"] == "test_telegram_token"
        assert config["twilio_sid"] == "test_sid"
        assert config["twilio_token"] == "test_token"
        assert config["twilio_number"] == "+15555555555"

    def test_load_config_empty(self):
        """Test loading config with no configuration."""
        with patch("clud.messaging.config.get_messaging_config_file") as mock_file:
            mock_file.return_value.exists.return_value = False
            config = load_messaging_config()
            # Should return empty dict or minimal config
            assert isinstance(config, dict)


@pytest.mark.asyncio
class TestTelegramClient:
    """Test Telegram client functionality."""

    async def test_send_message_success(self):
        """Test successful message sending."""
        from clud.messaging.telegram_client import TelegramClient

        client = TelegramClient("fake_token")
        if not client.is_available():
            pytest.skip("python-telegram-bot not installed")

        with patch.object(client, "bot") as mock_bot:
            mock_bot.send_message = AsyncMock(return_value=True)
            result = await client.send_message("123456789", "Test message")
            assert result is True

    async def test_send_code_block(self):
        """Test sending code block."""
        from clud.messaging.telegram_client import TelegramClient

        client = TelegramClient("fake_token")
        if not client.is_available():
            pytest.skip("python-telegram-bot not installed")

        with patch.object(client, "bot") as mock_bot:
            mock_bot.send_message = AsyncMock(return_value=True)
            result = await client.send_code_block("123456789", "print('hello')", "python")
            assert result is True
            # Verify formatted correctly
            call_args = mock_bot.send_message.call_args
            assert "```python" in str(call_args)


@pytest.mark.asyncio
class TestTwilioClient:
    """Test Twilio client functionality."""

    async def test_send_sms_success(self):
        """Test successful SMS sending."""
        from clud.messaging.twilio_client import TwilioClient

        client = TwilioClient("ACfake", "fake_token", "+15555555555")
        if not client.is_available():
            pytest.skip("twilio not installed")

        with patch.object(client.client, "messages") as mock_messages:
            mock_messages.create = Mock(return_value=Mock(sid="SMfake"))
            result = await client.send_message("+1234567890", "Test SMS")
            assert result is True

    async def test_send_whatsapp_success(self):
        """Test successful WhatsApp sending."""
        from clud.messaging.twilio_client import TwilioClient

        client = TwilioClient("ACfake", "fake_token", "+15555555555")
        if not client.is_available():
            pytest.skip("twilio not installed")

        with patch.object(client.client, "messages") as mock_messages:
            mock_messages.create = Mock(return_value=Mock(sid="SMfake"))
            result = await client.send_message("whatsapp:+1234567890", "Test WhatsApp")
            assert result is True

    async def test_message_truncation(self):
        """Test SMS message truncation."""
        from clud.messaging.twilio_client import TwilioClient

        client = TwilioClient("ACfake", "fake_token", "+15555555555")
        if not client.is_available():
            pytest.skip("twilio not installed")

        # Create a message longer than 1600 characters
        long_message = "x" * 2000

        with patch.object(client.client, "messages") as mock_messages:
            mock_messages.create = Mock(return_value=Mock(sid="SMfake"))
            result = await client.send_message("+1234567890", long_message)
            assert result is True
            # Verify message was truncated
            call_args = mock_messages.create.call_args
            sent_message = call_args.kwargs["body"]
            assert len(sent_message) <= 1620  # 1600 + truncation message


@pytest.mark.asyncio
class TestAgentNotifier:
    """Test AgentNotifier functionality."""

    async def test_notify_start(self):
        """Test start notification."""
        from clud.messaging.notifier import AgentNotifier

        mock_client = Mock()
        mock_client.send_message = AsyncMock(return_value=True)

        notifier = AgentNotifier(mock_client, "@testuser", update_interval=30)
        await notifier.notify_start("Test task")

        mock_client.send_message.assert_called_once()
        call_args = mock_client.send_message.call_args
        assert "Clud Agent Starting" in call_args[0][1]
        assert "Test task" in call_args[0][1]

    async def test_notify_progress_rate_limiting(self):
        """Test progress notification rate limiting."""
        from clud.messaging.notifier import AgentNotifier

        mock_client = Mock()
        mock_client.send_message = AsyncMock(return_value=True)

        notifier = AgentNotifier(mock_client, "@testuser", update_interval=10)

        # First call should send
        await notifier.notify_progress("Status 1")
        assert mock_client.send_message.call_count == 1

        # Immediate second call should not send (rate limited)
        await notifier.notify_progress("Status 2")
        assert mock_client.send_message.call_count == 1

    async def test_notify_completion_success(self):
        """Test completion notification for success."""
        from clud.messaging.notifier import AgentNotifier

        mock_client = Mock()
        mock_client.send_message = AsyncMock(return_value=True)

        notifier = AgentNotifier(mock_client, "@testuser")
        await notifier.notify_completion(success=True, summary="All tests passed")

        call_args = mock_client.send_message.call_args
        message = call_args[0][1]
        assert "✅" in message
        assert "Completed Successfully" in message
        assert "All tests passed" in message

    async def test_notify_completion_failure(self):
        """Test completion notification for failure."""
        from clud.messaging.notifier import AgentNotifier

        mock_client = Mock()
        mock_client.send_message = AsyncMock(return_value=True)

        notifier = AgentNotifier(mock_client, "@testuser")
        await notifier.notify_completion(success=False)

        call_args = mock_client.send_message.call_args
        message = call_args[0][1]
        assert "❌" in message
        assert "Failed" in message

    async def test_notify_error(self):
        """Test error notification."""
        from clud.messaging.notifier import AgentNotifier

        mock_client = Mock()
        mock_client.send_message = AsyncMock(return_value=True)

        notifier = AgentNotifier(mock_client, "@testuser")
        await notifier.notify_error("Test error message")

        call_args = mock_client.send_message.call_args
        message = call_args[0][1]
        assert "⚠️" in message
        assert "Error" in message
        assert "Test error message" in message

    async def test_notify_error_truncation(self):
        """Test error message truncation."""
        from clud.messaging.notifier import AgentNotifier

        mock_client = Mock()
        mock_client.send_message = AsyncMock(return_value=True)

        notifier = AgentNotifier(mock_client, "@testuser")
        long_error = "x" * 1000
        await notifier.notify_error(long_error)

        call_args = mock_client.send_message.call_args
        message = call_args[0][1]
        assert "[truncated]" in message
        assert len(message) < len(long_error) + 100  # Should be truncated
