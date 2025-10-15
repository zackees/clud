"""Unit tests for TelegramBotHandler.

Tests the Telegram bot handler's command processing, message handling,
user authentication, and error handling.
"""

# pyright: reportUnknownMemberType=false, reportAttributeAccessIssue=false
# Mock objects from unittest.mock have incomplete type stubs

import unittest
from datetime import datetime
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from telegram import Chat, Message, Update, User
from telegram.ext import ContextTypes

from clud.telegram.bot_handler import TelegramBotHandler
from clud.telegram.config import (
    SessionConfig,
    TelegramConfig,
    TelegramIntegrationConfig,
    WebConfig,
)
from clud.telegram.models import ContentType, MessageSender, TelegramMessage, TelegramSession
from clud.telegram.session_manager import SessionManager

pytestmark = pytest.mark.anyio


class TestTelegramBotHandler(unittest.TestCase):
    """Test cases for TelegramBotHandler."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create config
        self.config = TelegramIntegrationConfig(
            telegram=TelegramConfig(
                bot_token="test_token_123",
                webhook_url=None,
                allowed_users=[],
                polling=True,
            ),
            web=WebConfig(port=8889, host="127.0.0.1", auth_required=False, bidirectional=False),
            sessions=SessionConfig(timeout_seconds=3600, max_sessions=50, message_history_limit=1000, cleanup_interval=300),
        )

        # Create mock session manager
        self.session_manager = MagicMock(spec=SessionManager)

        # Create bot handler
        self.bot_handler = TelegramBotHandler(config=self.config, session_manager=self.session_manager)

        # Create mock update and context
        self.user = User(id=12345, first_name="John", last_name="Doe", is_bot=False, username="johndoe")
        self.chat = MagicMock(spec=Chat)
        self.chat.id = 12345
        self.chat.send_action = AsyncMock()
        self.message = MagicMock(spec=Message)
        self.message.message_id = 1
        self.message.text = "Hello bot"
        self.message.reply_text = AsyncMock()
        self.message.chat = self.chat
        self.update = MagicMock(spec=Update)
        self.update.effective_user = self.user
        self.update.message = self.message
        self.context = MagicMock(spec=ContextTypes.DEFAULT_TYPE)

    async def test_init(self) -> None:
        """Test TelegramBotHandler initialization."""
        handler = TelegramBotHandler(config=self.config, session_manager=self.session_manager)

        assert handler.config == self.config
        assert handler.session_manager == self.session_manager
        assert handler.application is None

    async def test_start_command_authorized_user(self) -> None:
        """Test /start command with authorized user."""
        # Setup
        session = TelegramSession(
            session_id="test-session-id",
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
            instance_id="test-instance",
            message_history=[],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )
        self.session_manager.get_or_create_session = AsyncMock(return_value=session)

        # Execute
        await self.bot_handler.start_command(self.update, self.context)

        # Verify
        self.session_manager.get_or_create_session.assert_called_once_with(
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
        )
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "Welcome to Claude Code" in call_args
        assert "/help" in call_args

    async def test_start_command_unauthorized_user(self) -> None:
        """Test /start command with unauthorized user."""
        # Setup - enable whitelist
        self.bot_handler.config.telegram.allowed_users = [99999]

        # Execute
        await self.bot_handler.start_command(self.update, self.context)

        # Verify
        self.session_manager.get_or_create_session.assert_not_called()
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "not authorized" in call_args

    async def test_start_command_no_user(self) -> None:
        """Test /start command with no user in update."""
        # Setup
        self.update.effective_user = None

        # Execute
        await self.bot_handler.start_command(self.update, self.context)

        # Verify
        self.session_manager.get_or_create_session.assert_not_called()
        self.message.reply_text.assert_not_called()

    async def test_start_command_session_creation_error(self) -> None:
        """Test /start command with session creation error."""
        # Setup
        self.session_manager.get_or_create_session = AsyncMock(side_effect=Exception("Database error"))

        # Execute
        await self.bot_handler.start_command(self.update, self.context)

        # Verify
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "error occurred" in call_args

    async def test_help_command(self) -> None:
        """Test /help command."""
        # Execute
        await self.bot_handler.help_command(self.update, self.context)

        # Verify
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args
        assert "Claude Code Bot Help" in call_args[0][0]
        assert call_args[1]["parse_mode"] == "Markdown"

    async def test_help_command_no_message(self) -> None:
        """Test /help command with no message in update."""
        # Setup
        self.update.message = None

        # Execute
        await self.bot_handler.help_command(self.update, self.context)

        # Verify - no reply should be sent
        # Since self.message.reply_text is still the mock from setUp,
        # we can't assert it wasn't called (there's no message to reply to)

    async def test_status_command_with_session(self) -> None:
        """Test /status command with active session."""
        # Setup
        session = TelegramSession(
            session_id="test-session-id-123",
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
            instance_id="test-instance",
            message_history=[
                TelegramMessage(
                    message_id="msg1",
                    session_id="test-session-id-123",
                    telegram_message_id=1,
                    sender=MessageSender.USER,
                    content="Hello",
                    content_type=ContentType.TEXT,
                    timestamp=datetime.now(),
                    metadata={},
                ),
                TelegramMessage(
                    message_id="msg2",
                    session_id="test-session-id-123",
                    telegram_message_id=2,
                    sender=MessageSender.BOT,
                    content="Hi",
                    content_type=ContentType.TEXT,
                    timestamp=datetime.now(),
                    metadata={},
                ),
            ],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )
        self.session_manager.get_user_session = MagicMock(return_value=session)

        # Execute
        await self.bot_handler.status_command(self.update, self.context)

        # Verify
        self.session_manager.get_user_session.assert_called_once_with(12345)
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args
        assert "Session Status" in call_args[0][0]
        assert "test-session" in call_args[0][0]
        assert call_args[1]["parse_mode"] == "Markdown"

    async def test_status_command_no_session(self) -> None:
        """Test /status command with no active session."""
        # Setup
        self.session_manager.get_user_session = MagicMock(return_value=None)

        # Execute
        await self.bot_handler.status_command(self.update, self.context)

        # Verify
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "No active session" in call_args

    async def test_clear_command_with_session(self) -> None:
        """Test /clear command with active session."""
        # Setup
        session = TelegramSession(
            session_id="test-session-id",
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
            instance_id="test-instance",
            message_history=[
                TelegramMessage(
                    message_id="msg1",
                    session_id="test-session-id",
                    telegram_message_id=1,
                    sender=MessageSender.USER,
                    content="Hello",
                    content_type=ContentType.TEXT,
                    timestamp=datetime.now(),
                    metadata={},
                )
            ],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )
        self.session_manager.get_user_session = MagicMock(return_value=session)

        # Execute
        await self.bot_handler.clear_command(self.update, self.context)

        # Verify
        assert len(session.message_history) == 0
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "cleared" in call_args.lower()

    async def test_clear_command_no_session(self) -> None:
        """Test /clear command with no active session."""
        # Setup
        self.session_manager.get_user_session = MagicMock(return_value=None)

        # Execute
        await self.bot_handler.clear_command(self.update, self.context)

        # Verify
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "No active session" in call_args

    async def test_handle_message_authorized_user(self) -> None:
        """Test handling regular message from authorized user."""
        # Setup
        session = TelegramSession(
            session_id="test-session-id",
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
            instance_id="test-instance",
            message_history=[],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )
        self.session_manager.get_user_session = MagicMock(return_value=session)
        self.session_manager.process_user_message = AsyncMock(return_value="Bot response")
        self.message.text = "What is Python?"

        # Execute
        await self.bot_handler.handle_message(self.update, self.context)

        # Verify
        self.chat.send_action.assert_called_once_with(action="typing")
        self.session_manager.process_user_message.assert_called_once_with(session_id="test-session-id", message_content="What is Python?", telegram_message_id=1)
        self.message.reply_text.assert_called_once_with("Bot response")

    async def test_handle_message_unauthorized_user(self) -> None:
        """Test handling message from unauthorized user."""
        # Setup - enable whitelist
        self.bot_handler.config.telegram.allowed_users = [99999]
        self.message.text = "Hello"

        # Execute
        await self.bot_handler.handle_message(self.update, self.context)

        # Verify
        self.session_manager.get_user_session.assert_not_called()
        self.session_manager.process_user_message.assert_not_called()
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "not authorized" in call_args

    async def test_handle_message_creates_session_if_needed(self) -> None:
        """Test that handle_message creates session if it doesn't exist."""
        # Setup
        self.session_manager.get_user_session = MagicMock(return_value=None)
        new_session = TelegramSession(
            session_id="new-session-id",
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
            instance_id="test-instance",
            message_history=[],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )
        self.session_manager.get_or_create_session = AsyncMock(return_value=new_session)
        self.session_manager.process_user_message = AsyncMock(return_value="Bot response")
        self.message.text = "Hello"

        # Execute
        await self.bot_handler.handle_message(self.update, self.context)

        # Verify
        self.session_manager.get_or_create_session.assert_called_once()
        self.session_manager.process_user_message.assert_called_once_with(session_id="new-session-id", message_content="Hello", telegram_message_id=1)

    async def test_handle_message_splits_long_response(self) -> None:
        """Test that long responses are split into chunks."""
        # Setup
        session = TelegramSession(
            session_id="test-session-id",
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
            instance_id="test-instance",
            message_history=[],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )
        self.session_manager.get_user_session = MagicMock(return_value=session)
        # Create a response longer than 4096 characters
        long_response = "A" * 5000
        self.session_manager.process_user_message = AsyncMock(return_value=long_response)
        self.message.text = "Tell me a long story"

        # Execute
        await self.bot_handler.handle_message(self.update, self.context)

        # Verify - should be called twice (two chunks)
        assert self.message.reply_text.call_count == 2
        # First chunk should be 4096 chars
        first_call_text = self.message.reply_text.call_args_list[0][0][0]
        assert len(first_call_text) == 4096

    async def test_handle_message_no_text(self) -> None:
        """Test handling update with no text."""
        # Setup
        self.update.message.text = None

        # Execute
        await self.bot_handler.handle_message(self.update, self.context)

        # Verify
        self.session_manager.get_user_session.assert_not_called()

    async def test_handle_message_processing_error(self) -> None:
        """Test error handling during message processing."""
        # Setup
        session = TelegramSession(
            session_id="test-session-id",
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
            instance_id="test-instance",
            message_history=[],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )
        self.session_manager.get_user_session = MagicMock(return_value=session)
        self.session_manager.process_user_message = AsyncMock(side_effect=Exception("Processing error"))
        self.message.text = "Hello"

        # Execute
        await self.bot_handler.handle_message(self.update, self.context)

        # Verify
        self.message.reply_text.assert_called_once()
        call_args = self.message.reply_text.call_args[0][0]
        assert "error occurred" in call_args

    async def test_error_handler(self) -> None:
        """Test error handler."""
        # Setup
        self.context.error = Exception("Test error")

        # Execute - should not raise
        await self.bot_handler.error_handler(self.update, self.context)

        # Verify - just check it doesn't crash
        # (error is logged but we can't easily verify logging)

    async def test_is_user_allowed_no_whitelist(self) -> None:
        """Test user authorization with no whitelist."""
        # Setup - empty allowed_users list
        self.bot_handler.config.telegram.allowed_users = []

        # Execute & Verify
        assert self.bot_handler._is_user_allowed(12345) is True
        assert self.bot_handler._is_user_allowed(99999) is True

    async def test_is_user_allowed_with_whitelist(self) -> None:
        """Test user authorization with whitelist."""
        # Setup
        self.bot_handler.config.telegram.allowed_users = [12345, 67890]

        # Execute & Verify
        assert self.bot_handler._is_user_allowed(12345) is True
        assert self.bot_handler._is_user_allowed(67890) is True
        assert self.bot_handler._is_user_allowed(99999) is False

    async def test_format_duration_seconds(self) -> None:
        """Test duration formatting for seconds."""
        result = self.bot_handler._format_duration(45)
        assert result == "45s"

    async def test_format_duration_minutes(self) -> None:
        """Test duration formatting for minutes."""
        result = self.bot_handler._format_duration(150)
        assert result == "2m"

    async def test_format_duration_hours(self) -> None:
        """Test duration formatting for hours."""
        result = self.bot_handler._format_duration(7350)
        assert result == "2h 2m"

    async def test_start_polling_success(self) -> None:
        """Test starting bot in polling mode."""
        with patch("clud.telegram.bot_handler.Application") as mock_app_class:
            # Setup mocks
            mock_builder = MagicMock()
            mock_app_class.builder.return_value = mock_builder
            mock_builder.token.return_value = mock_builder
            mock_app = AsyncMock()
            mock_builder.build.return_value = mock_app
            mock_app.initialize = AsyncMock()
            mock_app.start = AsyncMock()
            mock_updater = MagicMock()
            mock_updater.start_polling = AsyncMock()
            mock_app.updater = mock_updater

            # Execute
            await self.bot_handler.start_polling()

            # Verify
            mock_app_class.builder.assert_called_once()
            mock_builder.token.assert_called_once_with("test_token_123")
            mock_app.initialize.assert_called_once()
            mock_app.start.assert_called_once()
            mock_updater.start_polling.assert_called_once_with(drop_pending_updates=True)
            assert self.bot_handler.application == mock_app

    async def test_start_polling_failure(self) -> None:
        """Test start polling with error."""
        with patch("clud.telegram.bot_handler.Application") as mock_app_class:
            # Setup mocks to raise error
            mock_builder = MagicMock()
            mock_app_class.builder.return_value = mock_builder
            mock_builder.token.return_value = mock_builder
            mock_app = AsyncMock()
            mock_builder.build.return_value = mock_app
            mock_app.initialize = AsyncMock(side_effect=Exception("Connection error"))

            # Execute & Verify
            with pytest.raises(RuntimeError, match="Failed to start Telegram bot"):
                await self.bot_handler.start_polling()

    async def test_stop_success(self) -> None:
        """Test stopping bot successfully."""
        # Setup
        mock_app = AsyncMock()
        mock_updater = MagicMock()
        mock_updater.stop = AsyncMock()
        mock_app.updater = mock_updater
        mock_app.stop = AsyncMock()
        mock_app.shutdown = AsyncMock()
        self.bot_handler.application = mock_app

        # Execute
        await self.bot_handler.stop()

        # Verify
        mock_updater.stop.assert_called_once()
        mock_app.stop.assert_called_once()
        mock_app.shutdown.assert_called_once()

    async def test_stop_with_error(self) -> None:
        """Test stopping bot with error."""
        # Setup
        mock_app = AsyncMock()
        mock_updater = MagicMock()
        mock_updater.stop = AsyncMock(side_effect=Exception("Stop error"))
        mock_app.updater = mock_updater
        self.bot_handler.application = mock_app

        # Execute - should not raise
        await self.bot_handler.stop()

        # Verify - error is logged but doesn't propagate

    async def test_stop_no_application(self) -> None:
        """Test stopping when no application is running."""
        # Setup
        self.bot_handler.application = None

        # Execute - should not raise
        await self.bot_handler.stop()

        # Verify - nothing should happen


if __name__ == "__main__":
    unittest.main()
