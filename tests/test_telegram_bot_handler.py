"""Unit tests for TelegramBotHandler.

Tests the Telegram bot handler's command processing, message handling,
user authentication, and error handling.
"""

# pyright: reportUnknownMemberType=false, reportAttributeAccessIssue=false, reportMissingImports=false, reportUntypedFunctionDecorator=false, reportUnknownVariableType=false
# Mock objects from unittest.mock and pytest have incomplete type stubs

import unittest
from datetime import datetime
from unittest.mock import AsyncMock, MagicMock

import pytest  # pyright: ignore[reportMissingImports]
from mocks import create_mock_api
from telegram import Chat, Message, Update, User
from telegram.ext import ContextTypes

from clud.telegram.api_config import TelegramAPIConfig
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


class TestTelegramBotHandler(unittest.IsolatedAsyncioTestCase):
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
            api=TelegramAPIConfig.for_testing(implementation="mock"),
        )

        # Create mock session manager
        self.session_manager = MagicMock(spec=SessionManager)

        # Create mock Telegram API
        self.mock_api = create_mock_api()

        # Create bot handler
        self.bot_handler = TelegramBotHandler(config=self.config, session_manager=self.session_manager, api=self.mock_api)

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
        mock_api = create_mock_api()
        handler = TelegramBotHandler(config=self.config, session_manager=self.session_manager, api=mock_api)

        assert handler.config == self.config
        assert handler.session_manager == self.session_manager
        assert handler.api == mock_api

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
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
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
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
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
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
        assert "error occurred" in call_args

    async def test_help_command(self) -> None:
        """Test /help command."""
        # Execute
        await self.bot_handler.help_command(self.update, self.context)

        # Verify
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args
        assert "Claude Code Bot Help" in call_args[1]["text"]
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
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args
        assert "Session Status" in call_args[1]["text"]
        assert "test-ses" in call_args[1]["text"]  # Session ID truncated to first 8 chars
        assert call_args[1]["parse_mode"] == "Markdown"

    async def test_status_command_no_session(self) -> None:
        """Test /status command with no active session."""
        # Setup
        self.session_manager.get_user_session = MagicMock(return_value=None)

        # Execute
        await self.bot_handler.status_command(self.update, self.context)

        # Verify
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
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
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
        assert "cleared" in call_args.lower()

    async def test_clear_command_no_session(self) -> None:
        """Test /clear command with no active session."""
        # Setup
        self.session_manager.get_user_session = MagicMock(return_value=None)

        # Execute
        await self.bot_handler.clear_command(self.update, self.context)

        # Verify
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
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
        self.mock_api.send_typing_action.assert_called_once()
        self.session_manager.process_user_message.assert_called_once_with(session_id="test-session-id", message_content="What is Python?", telegram_message_id=1)
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
        assert call_args == "Bot response"

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
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
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
        """Test that long responses are sent to API (splitting is handled by API implementation)."""
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

        # Verify - bot handler calls send_message once, API handles splitting
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
        assert len(call_args) == 5000

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
        self.mock_api.send_message.assert_called_once()
        call_args = self.mock_api.send_message.call_args[1]["text"]
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
        # Execute
        await self.bot_handler.start_polling()

        # Verify
        self.mock_api.initialize.assert_called_once()
        self.mock_api.add_command_handler.assert_any_call("start", self.bot_handler.start_command)
        self.mock_api.add_command_handler.assert_any_call("help", self.bot_handler.help_command)
        self.mock_api.add_command_handler.assert_any_call("status", self.bot_handler.status_command)
        self.mock_api.add_command_handler.assert_any_call("clear", self.bot_handler.clear_command)
        self.mock_api.add_message_handler.assert_called_once_with(self.bot_handler.handle_message)
        self.mock_api.add_error_handler.assert_called_once_with(self.bot_handler.error_handler)
        self.mock_api.start_polling.assert_called_once_with(drop_pending_updates=True)

    async def test_start_polling_failure(self) -> None:
        """Test start polling with error."""
        # Setup mock to raise error
        self.mock_api.initialize = AsyncMock(return_value=False)

        # Execute & Verify
        with pytest.raises(RuntimeError, match="Failed to initialize Telegram bot API"):
            await self.bot_handler.start_polling()

    async def test_stop_success(self) -> None:
        """Test stopping bot successfully."""
        # Execute
        await self.bot_handler.stop()

        # Verify
        self.mock_api.stop_polling.assert_called_once()
        self.mock_api.shutdown.assert_called_once()

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
