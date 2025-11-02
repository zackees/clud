"""Integration tests for TelegramBotHandler with FakeTelegramBotAPI.

Tests the complete integration between TelegramBotHandler and FakeTelegramBotAPI,
verifying that commands, message handling, and error scenarios work correctly
with the abstract API layer.
"""

import unittest
from datetime import datetime
from unittest.mock import AsyncMock, MagicMock

from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_fake import FakeTelegramBotAPI
from clud.telegram.api_interface import TelegramUser
from clud.telegram.bot_handler import TelegramBotHandler
from clud.telegram.config import (
    SessionConfig,
    TelegramConfig,
    TelegramIntegrationConfig,
    WebConfig,
)
from clud.telegram.models import ContentType, MessageSender, TelegramMessage, TelegramSession
from clud.telegram.session_manager import SessionManager


class TestTelegramBotHandlerIntegration(unittest.IsolatedAsyncioTestCase):
    """Integration test cases for TelegramBotHandler with FakeTelegramBotAPI."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create fake API config
        api_config = TelegramAPIConfig.for_testing(implementation="fake")

        # Create config
        self.config = TelegramIntegrationConfig(
            telegram=TelegramConfig(
                bot_token="test_token_integration",
                webhook_url=None,
                allowed_users=[],
                polling=True,
            ),
            web=WebConfig(port=8889, host="127.0.0.1", auth_required=False, bidirectional=False),
            sessions=SessionConfig(timeout_seconds=3600, max_sessions=50, message_history_limit=1000, cleanup_interval=300),
            api=api_config,
        )

        # Create fake API
        self.fake_api = FakeTelegramBotAPI(api_config)

        # Create mock session manager
        self.session_manager = MagicMock(spec=SessionManager)

        # Create bot handler with fake API
        self.bot_handler = TelegramBotHandler(config=self.config, session_manager=self.session_manager, api=self.fake_api)

        # Create test user
        self.test_user = TelegramUser(
            id=12345,
            username="johndoe",
            first_name="John",
            last_name="Doe",
            is_bot=False,
        )

        # Test chat ID
        self.test_chat_id = 12345

    async def asyncSetUp(self) -> None:
        """Async setup - initialize fake API and register handlers."""
        await self.fake_api.initialize()

        # Register handlers (normally done in start_polling, but we do it manually for tests)
        self.fake_api.add_command_handler("start", self.bot_handler.start_command)
        self.fake_api.add_command_handler("help", self.bot_handler.help_command)
        self.fake_api.add_command_handler("status", self.bot_handler.status_command)
        self.fake_api.add_command_handler("clear", self.bot_handler.clear_command)
        self.fake_api.add_message_handler(self.bot_handler.handle_message)
        self.fake_api.add_error_handler(self.bot_handler.error_handler)

    async def asyncTearDown(self) -> None:
        """Async teardown - cleanup fake API."""
        self.fake_api.clear_history()
        await self.fake_api.shutdown()

    async def _process_pending_updates(self) -> None:
        """Helper to process all pending updates in the queue."""
        while not self.fake_api._pending_updates.empty():
            update = await self.fake_api._pending_updates.get()
            await self.fake_api._process_update(update)

    # /start command tests

    async def test_start_command_authorized_user(self) -> None:
        """Test /start command with authorized user sends welcome message."""
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

        # Execute - simulate /start command and process it
        await self.fake_api.simulate_command(self.test_chat_id, "/start", self.test_user)
        await self._process_pending_updates()

        # Verify session was created
        self.session_manager.get_or_create_session.assert_called_once_with(
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
        )

        # Verify welcome message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("Welcome to Claude Code", messages[0].text)
        self.assertIn("/help", messages[0].text)

    async def test_start_command_unauthorized_user(self) -> None:
        """Test /start command with unauthorized user sends rejection message."""
        # Setup - enable whitelist with different user ID
        self.bot_handler.config.telegram.allowed_users = [99999]

        # Execute - simulate /start command
        await self.fake_api.simulate_command(self.test_chat_id, "/start", self.test_user)
        await self._process_pending_updates()

        # Verify session was NOT created
        self.session_manager.get_or_create_session.assert_not_called()

        # Verify rejection message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("not authorized", messages[0].text)

    async def test_start_command_session_creation_error(self) -> None:
        """Test /start command with session creation error sends error message."""
        # Setup - mock session manager to raise exception
        self.session_manager.get_or_create_session = AsyncMock(side_effect=Exception("Database error"))

        # Execute - simulate /start command
        await self.fake_api.simulate_command(self.test_chat_id, "/start", self.test_user)
        await self._process_pending_updates()

        # Verify error message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("error occurred", messages[0].text)

    # /help command tests

    async def test_help_command(self) -> None:
        """Test /help command sends help text with commands."""
        # Execute - simulate /help command
        await self.fake_api.simulate_command(self.test_chat_id, "/help", self.test_user)
        await self._process_pending_updates()

        # Verify help message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("Claude Code Bot Help", messages[0].text)
        self.assertIn("/start", messages[0].text)
        self.assertIn("/clear", messages[0].text)
        self.assertIn("/status", messages[0].text)
        self.assertEqual(messages[0].parse_mode, "Markdown")

    # /status command tests

    async def test_status_command_with_session(self) -> None:
        """Test /status command with active session shows session info."""
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

        # Execute - simulate /status command
        await self.fake_api.simulate_command(self.test_chat_id, "/status", self.test_user)
        await self._process_pending_updates()

        # Verify status message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("Session Status", messages[0].text)
        self.assertIn("test-ses", messages[0].text)  # Session ID truncated
        self.assertIn("johndoe", messages[0].text)
        self.assertEqual(messages[0].parse_mode, "Markdown")

    async def test_status_command_no_session(self) -> None:
        """Test /status command with no active session sends no session message."""
        # Setup
        self.session_manager.get_user_session = MagicMock(return_value=None)

        # Execute - simulate /status command
        await self.fake_api.simulate_command(self.test_chat_id, "/status", self.test_user)
        await self._process_pending_updates()

        # Verify no session message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("No active session", messages[0].text)

    # /clear command tests

    async def test_clear_command_with_session(self) -> None:
        """Test /clear command with active session clears message history."""
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

        # Execute - simulate /clear command
        await self.fake_api.simulate_command(self.test_chat_id, "/clear", self.test_user)
        await self._process_pending_updates()

        # Verify message history was cleared
        self.assertEqual(len(session.message_history), 0)

        # Verify confirmation message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("cleared", messages[0].text.lower())

    async def test_clear_command_no_session(self) -> None:
        """Test /clear command with no active session sends no session message."""
        # Setup
        self.session_manager.get_user_session = MagicMock(return_value=None)

        # Execute - simulate /clear command
        await self.fake_api.simulate_command(self.test_chat_id, "/clear", self.test_user)
        await self._process_pending_updates()

        # Verify no session message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("No active session", messages[0].text)

    # Message handling tests

    async def test_handle_message_authorized_user(self) -> None:
        """Test handling regular message from authorized user processes and responds."""
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
        self.session_manager.process_user_message = AsyncMock(return_value="Bot response: Python is a programming language.")

        # Execute - simulate regular message
        await self.fake_api.simulate_incoming_message(self.test_chat_id, "What is Python?", self.test_user)
        await self._process_pending_updates()

        # Verify typing action was sent
        self.assertTrue(self.fake_api.was_typing_sent(self.test_chat_id))

        # Verify message was processed
        self.session_manager.process_user_message.assert_called_once()
        call_args = self.session_manager.process_user_message.call_args
        self.assertEqual(call_args.kwargs["session_id"], "test-session-id")
        self.assertEqual(call_args.kwargs["message_content"], "What is Python?")

        # Verify response was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertEqual(messages[0].text, "Bot response: Python is a programming language.")

    async def test_handle_message_unauthorized_user(self) -> None:
        """Test handling message from unauthorized user sends rejection message."""
        # Setup - enable whitelist
        self.bot_handler.config.telegram.allowed_users = [99999]

        # Execute - simulate message
        await self.fake_api.simulate_incoming_message(self.test_chat_id, "Hello", self.test_user)
        await self._process_pending_updates()

        # Verify session was NOT accessed
        self.session_manager.get_user_session.assert_not_called()
        self.session_manager.process_user_message.assert_not_called()

        # Verify rejection message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("not authorized", messages[0].text)

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

        # Execute - simulate message
        await self.fake_api.simulate_incoming_message(self.test_chat_id, "Hello", self.test_user)
        await self._process_pending_updates()

        # Verify session was created
        self.session_manager.get_or_create_session.assert_called_once_with(
            telegram_user_id=12345,
            telegram_username="johndoe",
            telegram_first_name="John",
            telegram_last_name="Doe",
        )

        # Verify message was processed with new session
        self.session_manager.process_user_message.assert_called_once()
        call_args = self.session_manager.process_user_message.call_args
        self.assertEqual(call_args.kwargs["session_id"], "new-session-id")

    async def test_handle_message_processing_error(self) -> None:
        """Test error handling during message processing sends error message."""
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

        # Execute - simulate message
        await self.fake_api.simulate_incoming_message(self.test_chat_id, "Hello", self.test_user)
        await self._process_pending_updates()

        # Verify error message was sent
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("error occurred", messages[0].text)

    # Handler registration tests

    async def test_start_polling_registers_handlers(self) -> None:
        """Test that start_polling registers all command and message handlers."""
        # Note: Handlers are already registered in asyncSetUp for other tests,
        # so we verify the counts account for both asyncSetUp and start_polling

        # Execute - start polling (which registers handlers)
        await self.bot_handler.start_polling()

        # Verify command handlers were registered (4 from asyncSetUp + 4 from start_polling = 8, but dict overwrites)
        # Actually, command handlers are stored in a dict, so they get overwritten - still 4
        self.assertEqual(self.fake_api.get_handler_count("command"), 4)  # start, help, status, clear

        # Verify message handlers were registered (message handlers are stored in a list, so they accumulate)
        # We have 1 from asyncSetUp + 1 from start_polling = 2
        self.assertEqual(self.fake_api.get_handler_count("message"), 2)

        # Verify error handlers were registered (error handlers are stored in a list, so they accumulate)
        # We have 1 from asyncSetUp + 1 from start_polling = 2
        self.assertEqual(self.fake_api.get_handler_count("error"), 2)

        # Cleanup
        await self.bot_handler.stop()

    async def test_stop_polling_shuts_down_cleanly(self) -> None:
        """Test that stop polling shuts down the bot cleanly."""
        # Setup - start polling first
        await self.bot_handler.start_polling()

        # Execute - stop polling
        await self.bot_handler.stop()

        # Verify API is no longer polling
        # Note: FakeTelegramBotAPI doesn't expose polling state directly,
        # but we can verify stop() doesn't raise exceptions

    # Multiple commands flow test

    async def test_command_flow_start_message_status_clear(self) -> None:
        """Test realistic flow: /start -> message -> /status -> /clear."""
        # Setup session manager for full flow
        session = TelegramSession(
            session_id="flow-test-session",
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
        self.session_manager.get_user_session = MagicMock(return_value=session)
        self.session_manager.process_user_message = AsyncMock(return_value="Response to your question")

        # Step 1: /start
        await self.fake_api.simulate_command(self.test_chat_id, "/start", self.test_user)
        await self._process_pending_updates()
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("Welcome", messages[0].text)

        # Step 2: Send a message
        await self.fake_api.simulate_incoming_message(self.test_chat_id, "What is Python?", self.test_user)
        await self._process_pending_updates()
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 2)  # Welcome + response
        self.assertEqual(messages[1].text, "Response to your question")

        # Step 3: /status
        await self.fake_api.simulate_command(self.test_chat_id, "/status", self.test_user)
        await self._process_pending_updates()
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 3)  # Welcome + response + status
        self.assertIn("Session Status", messages[2].text)

        # Step 4: /clear
        await self.fake_api.simulate_command(self.test_chat_id, "/clear", self.test_user)
        await self._process_pending_updates()
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 4)  # Welcome + response + status + clear confirmation
        self.assertIn("cleared", messages[3].text.lower())

        # Verify session history was cleared
        self.assertEqual(len(session.message_history), 0)

    # User authorization tests

    async def test_whitelist_enforcement_across_commands(self) -> None:
        """Test that whitelist is enforced for both commands and messages."""
        # Setup - whitelist only user 99999
        self.bot_handler.config.telegram.allowed_users = [99999]

        # Test /start
        await self.fake_api.simulate_command(self.test_chat_id, "/start", self.test_user)
        await self._process_pending_updates()
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("not authorized", messages[0].text)

        # Clear messages
        self.fake_api.clear_history()

        # Test regular message
        await self.fake_api.simulate_incoming_message(self.test_chat_id, "Hello", self.test_user)
        await self._process_pending_updates()
        messages = self.fake_api.get_sent_messages(self.test_chat_id)
        self.assertEqual(len(messages), 1)
        self.assertIn("not authorized", messages[0].text)

        # Verify session manager was never called
        self.session_manager.get_or_create_session.assert_not_called()
        self.session_manager.get_user_session.assert_not_called()
        self.session_manager.process_user_message.assert_not_called()


if __name__ == "__main__":
    unittest.main()
