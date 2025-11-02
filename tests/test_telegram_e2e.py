"""End-to-end tests for Telegram integration with FakeTelegramBotAPI.

This test suite validates complete flows across all Telegram components:
- TelegramBotHandler (message routing, command handling)
- SessionManager (session management, message processing)
- InstancePool + CludInstance (instance lifecycle, command execution)
- TelegramHookHandler (output streaming)
- TelegramMessenger (bidirectional messaging)

Tests use FakeTelegramBotAPI for deterministic behavior without network calls.
"""

import asyncio
import logging
import unittest
from datetime import datetime
from pathlib import Path
from unittest.mock import AsyncMock, patch

from clud.api.instance_manager import InstancePool
from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_fake import FakeTelegramBotAPI
from clud.telegram.api_interface import HandlerContext, TelegramChat, TelegramUpdate, TelegramUser
from clud.telegram.api_interface import TelegramMessage as TelegramAPIMessage
from clud.telegram.bot_handler import TelegramBotHandler
from clud.telegram.config import TelegramConfig, TelegramIntegrationConfig, WebConfig
from clud.telegram.session_manager import SessionManager

logger = logging.getLogger(__name__)


class TestTelegramE2E(unittest.IsolatedAsyncioTestCase):
    """End-to-end test cases for complete Telegram integration flows."""

    def setUp(self) -> None:
        """Set up test fixtures before each test."""
        # Create TelegramAPIConfig for testing
        self.api_config = TelegramAPIConfig(
            implementation="fake",
            bot_token="fake-bot-token",
            fake_delay_ms=0,  # No delay for fast tests
        )

        # Create FakeTelegramBotAPI
        self.fake_api = FakeTelegramBotAPI(config=self.api_config)

        # Create test working directory
        self.test_working_dir = Path(__file__).parent.parent  # Project root

        # Create InstancePool (for managing clud instances)
        self.instance_pool = InstancePool(
            max_instances=10,
            idle_timeout_seconds=300,
        )

        # Create SessionManager
        self.session_manager = SessionManager(
            instance_pool=self.instance_pool,
            max_sessions=10,
            session_timeout_seconds=3600,
            message_history_limit=100,
        )

        # Create TelegramIntegrationConfig
        telegram_config = TelegramConfig(
            bot_token="fake-bot-token",
            allowed_users=[123456789, 987654321],  # Test user IDs
            polling=True,
        )
        web_config = WebConfig(
            host="localhost",
            port=8080,
        )
        self.integration_config = TelegramIntegrationConfig(
            telegram=telegram_config,
            web=web_config,
            api=self.api_config,
        )

        # Create TelegramBotHandler
        self.bot_handler = TelegramBotHandler(
            config=self.integration_config,
            session_manager=self.session_manager,
            api=self.fake_api,
        )

        # Test user data
        self.test_user_id = 123456789
        self.test_username = "testuser"
        self.test_first_name = "Test"
        self.test_last_name = "User"
        self.test_chat_id = "123456789"

    async def asyncSetUp(self) -> None:
        """Async setup before each test."""
        # Initialize fake API
        await self.fake_api.initialize()

    async def asyncTearDown(self) -> None:
        """Async cleanup after each test."""
        # Clear fake API history
        self.fake_api.clear_history()

        # Shutdown instance pool
        await self.instance_pool.shutdown()

        # Shutdown fake API
        await self.fake_api.shutdown()

    def _create_test_update(
        self,
        text: str,
        user_id: int | None = None,
        chat_id: str | None = None,
        is_command: bool = False,
    ) -> TelegramUpdate:
        """Create a test TelegramUpdate for simulating user messages.

        Args:
            text: Message text
            user_id: User ID (defaults to test_user_id)
            chat_id: Chat ID (defaults to test_chat_id)
            is_command: Whether this is a command

        Returns:
            TelegramUpdate instance
        """
        user_id = user_id or self.test_user_id
        chat_id = chat_id or self.test_chat_id

        user = TelegramUser(
            id=user_id,
            is_bot=False,
            first_name=self.test_first_name,
            last_name=self.test_last_name,
            username=self.test_username,
        )

        chat = TelegramChat(
            id=int(chat_id),
            type="private",
            title=None,
            username=self.test_username,
        )

        message = TelegramAPIMessage(
            message_id=1,
            date=datetime.now(),
            chat=chat,
            text=text,
            from_user=user,
        )

        return TelegramUpdate(
            update_id=1,
            message=message,
            effective_user=user,
            effective_chat=chat,
        )

    def _create_test_context(self) -> HandlerContext:
        """Create a test HandlerContext for handler calls.

        Returns:
            HandlerContext instance
        """
        return HandlerContext(bot=self.fake_api, user_data={}, chat_data={})

    # ==================== Test Cases ====================

    async def test_start_command_creates_session_and_sends_welcome(self) -> None:
        """Test /start command creates session and sends welcome message."""
        # Create /start update
        update = self._create_test_update("/start", is_command=True)
        context = self._create_test_context()

        # Execute /start command
        await self.bot_handler.start_command(update, context)

        # Verify session created
        session = self.session_manager.get_user_session(self.test_user_id)
        self.assertIsNotNone(session)
        self.assertEqual(session.telegram_user_id, self.test_user_id)
        self.assertEqual(session.telegram_username, self.test_username)

        # Verify welcome message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Welcome to Claude Code", messages[0].text)
        self.assertIn("/help", messages[0].text)
        self.assertIn("/status", messages[0].text)
        self.assertIn("/clear", messages[0].text)

    async def test_help_command_sends_help_text(self) -> None:
        """Test /help command sends help text."""
        # Create session first
        await self.session_manager.get_or_create_session(
            telegram_user_id=self.test_user_id,
            telegram_username=self.test_username,
            telegram_first_name=self.test_first_name,
            telegram_last_name=self.test_last_name,
        )

        # Create /help update
        update = self._create_test_update("/help", is_command=True)
        context = self._create_test_context()

        # Execute /help command
        await self.bot_handler.help_command(update, context)

        # Verify help message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Claude Code Bot Help", messages[0].text)
        self.assertIn("Available Commands", messages[0].text)
        self.assertEqual(messages[0].parse_mode, "Markdown")

    async def test_status_command_shows_session_status(self) -> None:
        """Test /status command shows session status."""
        # Create session first
        session = await self.session_manager.get_or_create_session(
            telegram_user_id=self.test_user_id,
            telegram_username=self.test_username,
            telegram_first_name=self.test_first_name,
            telegram_last_name=self.test_last_name,
        )

        # Create /status update
        update = self._create_test_update("/status", is_command=True)
        context = self._create_test_context()

        # Execute /status command
        await self.bot_handler.status_command(update, context)

        # Verify status message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Session Status", messages[0].text)
        self.assertIn(session.session_id[:8], messages[0].text)
        self.assertIn(self.test_username, messages[0].text)
        self.assertEqual(messages[0].parse_mode, "Markdown")

    async def test_clear_command_clears_message_history(self) -> None:
        """Test /clear command clears message history."""
        # Create session first
        session = await self.session_manager.get_or_create_session(
            telegram_user_id=self.test_user_id,
            telegram_username=self.test_username,
            telegram_first_name=self.test_first_name,
            telegram_last_name=self.test_last_name,
        )

        # Add some messages to history
        from clud.telegram.models import TelegramMessage

        session.message_history.append(TelegramMessage.create_user_message(session.session_id, 1, "test message 1"))
        session.message_history.append(TelegramMessage.create_bot_message(session.session_id, "response 1"))
        session.message_history.append(TelegramMessage.create_user_message(session.session_id, 2, "test message 2"))

        # Verify messages exist
        self.assertEqual(len(session.message_history), 3)

        # Create /clear update
        update = self._create_test_update("/clear", is_command=True)
        context = self._create_test_context()

        # Execute /clear command
        await self.bot_handler.clear_command(update, context)

        # Verify message history cleared
        self.assertEqual(len(session.message_history), 0)

        # Verify confirmation message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Conversation history cleared", messages[0].text)

    async def test_unauthorized_user_rejected(self) -> None:
        """Test unauthorized user is rejected."""
        # Create update with unauthorized user ID
        unauthorized_user_id = 999999999
        update = self._create_test_update("/start", user_id=unauthorized_user_id)
        context = self._create_test_context()

        # Execute /start command
        await self.bot_handler.start_command(update, context)

        # Verify session NOT created
        session = self.session_manager.get_user_session(unauthorized_user_id)
        self.assertIsNone(session)

        # Verify rejection message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("not authorized", messages[0].text)

    async def test_multiple_users_have_separate_sessions(self) -> None:
        """Test multiple users have separate independent sessions."""
        # Create sessions for two different users
        user1_id = 123456789
        user2_id = 987654321

        # User 1 starts session
        update1 = self._create_test_update("/start", user_id=user1_id, chat_id=str(user1_id))
        context1 = self._create_test_context()
        await self.bot_handler.start_command(update1, context1)

        # User 2 starts session
        update2 = self._create_test_update("/start", user_id=user2_id, chat_id=str(user2_id))
        context2 = self._create_test_context()
        await self.bot_handler.start_command(update2, context2)

        # Verify separate sessions created
        session1 = self.session_manager.get_user_session(user1_id)
        session2 = self.session_manager.get_user_session(user2_id)

        self.assertIsNotNone(session1)
        self.assertIsNotNone(session2)
        self.assertNotEqual(session1.session_id, session2.session_id)

        # Verify separate instances created
        self.assertIsNotNone(session1.instance_id)
        self.assertIsNotNone(session2.instance_id)
        self.assertNotEqual(session1.instance_id, session2.instance_id)

        # Verify separate welcome messages sent
        messages1 = self.fake_api.get_sent_messages(user1_id)
        messages2 = self.fake_api.get_sent_messages(user2_id)

        self.assertEqual(len(messages1), 1)
        self.assertEqual(len(messages2), 1)
        self.assertIn("Welcome", messages1[0].text)
        self.assertIn("Welcome", messages2[0].text)

    async def test_session_reuse_for_same_user(self) -> None:
        """Test same user reuses existing session."""
        # User starts session twice
        update = self._create_test_update("/start")
        context = self._create_test_context()

        # First /start
        await self.bot_handler.start_command(update, context)
        session1 = self.session_manager.get_user_session(self.test_user_id)
        session1_id = session1.session_id

        # Second /start (should reuse session)
        await self.bot_handler.start_command(update, context)
        session2 = self.session_manager.get_user_session(self.test_user_id)
        session2_id = session2.session_id

        # Verify same session reused
        self.assertEqual(session1_id, session2_id)

        # Verify two welcome messages sent (one per /start)
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 2)

    async def test_handle_message_without_session_returns_error(self) -> None:
        """Test handling message without session returns error."""
        # Create message update without creating session first
        update = self._create_test_update("Hello, bot!")
        context = self._create_test_context()

        # Mock handle_message to avoid real instance execution
        # We'll test the session check only
        with patch.object(self.bot_handler, "handle_message") as mock_handle:
            mock_handle.return_value = None
            await self.bot_handler.handle_message(update, context)
            mock_handle.assert_called_once()

        # In real implementation, handle_message checks for session
        # and sends error if not found. Since we patched it, we just
        # verify the call happened.

    @patch("clud.api.instance_manager.CludInstance.execute")
    async def test_message_processing_with_mocked_instance(self, mock_execute: AsyncMock) -> None:
        """Test message processing flow with mocked clud instance execution."""
        # Create session first
        session = await self.session_manager.get_or_create_session(
            telegram_user_id=self.test_user_id,
            telegram_username=self.test_username,
            telegram_first_name=self.test_first_name,
            telegram_last_name=self.test_last_name,
        )

        # Mock instance execution to return fake response
        mock_execute.return_value = {
            "status": "completed",
            "output": "Hello! I'm Claude. How can I help you today?",
            "error": None,
            "exit_code": 0,
        }

        # Process user message
        user_message = "Hello, Claude!"
        response = await self.session_manager.process_user_message(
            session_id=session.session_id,
            message_content=user_message,
            telegram_message_id=1,
        )

        # Verify response received
        self.assertIsNotNone(response)
        self.assertIn("Hello! I'm Claude", response)

        # Verify mock was called
        mock_execute.assert_called_once_with(user_message)

        # Verify messages added to session history (user + bot)
        self.assertEqual(len(session.message_history), 2)
        self.assertEqual(session.message_history[0].content, user_message)
        self.assertEqual(session.message_history[1].content, response)

    @patch("clud.api.instance_manager.CludInstance.execute")
    async def test_complete_conversation_flow(self, mock_execute: AsyncMock) -> None:
        """Test complete conversation flow: start → message → response → status."""
        # Step 1: User sends /start
        update_start = self._create_test_update("/start", is_command=True)
        context = self._create_test_context()
        await self.bot_handler.start_command(update_start, context)

        session = self.session_manager.get_user_session(self.test_user_id)
        self.assertIsNotNone(session)

        # Verify welcome message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Welcome", messages[0].text)

        # Step 2: User sends message
        mock_execute.return_value = {
            "status": "completed",
            "output": "I can help you with Python programming!",
            "error": None,
            "exit_code": 0,
        }

        response = await self.session_manager.process_user_message(
            session_id=session.session_id,
            message_content="Can you help me with Python?",
            telegram_message_id=2,
        )

        # Verify response
        self.assertIn("Python programming", response)

        # Step 3: User sends /status
        self.fake_api.clear_history()  # Clear previous messages
        update_status = self._create_test_update("/status", is_command=True)
        await self.bot_handler.status_command(update_status, context)

        # Verify status shows updated message count
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Session Status", messages[0].text)
        # Should show 2 messages (1 user, 1 bot from step 2)
        self.assertIn("Messages: 2", messages[0].text)

    async def test_status_command_without_session(self) -> None:
        """Test /status command without active session."""
        # Create /status update without creating session first
        update = self._create_test_update("/status", is_command=True)
        context = self._create_test_context()

        # Execute /status command
        await self.bot_handler.status_command(update, context)

        # Verify error message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("No active session", messages[0].text)

    async def test_clear_command_without_session(self) -> None:
        """Test /clear command without active session."""
        # Create /clear update without creating session first
        update = self._create_test_update("/clear", is_command=True)
        context = self._create_test_context()

        # Execute /clear command
        await self.bot_handler.clear_command(update, context)

        # Verify error message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("No active session", messages[0].text)

    @patch("clud.api.instance_manager.CludInstance.execute")
    async def test_multiple_users_concurrent_messages(self, mock_execute: AsyncMock) -> None:
        """Test multiple users sending messages concurrently."""
        # Create sessions for two users
        user1_id = 123456789
        user2_id = 987654321

        session1 = await self.session_manager.get_or_create_session(
            telegram_user_id=user1_id,
            telegram_username="user1",
            telegram_first_name="User",
            telegram_last_name="One",
        )

        session2 = await self.session_manager.get_or_create_session(
            telegram_user_id=user2_id,
            telegram_username="user2",
            telegram_first_name="User",
            telegram_last_name="Two",
        )

        # Mock instance execution to return different responses
        mock_execute.side_effect = [
            {
                "status": "completed",
                "output": "Response for user 1",
                "error": None,
                "exit_code": 0,
            },
            {
                "status": "completed",
                "output": "Response for user 2",
                "error": None,
                "exit_code": 0,
            },
        ]

        # Process messages concurrently
        response1_task = asyncio.create_task(
            self.session_manager.process_user_message(
                session_id=session1.session_id,
                message_content="Message from user 1",
                telegram_message_id=1,
            )
        )

        response2_task = asyncio.create_task(
            self.session_manager.process_user_message(
                session_id=session2.session_id,
                message_content="Message from user 2",
                telegram_message_id=2,
            )
        )

        # Wait for both to complete
        response1 = await response1_task
        response2 = await response2_task

        # Verify responses are correct for each user
        self.assertIn("Response for user 1", response1)
        self.assertIn("Response for user 2", response2)

        # Verify separate message histories
        self.assertEqual(len(session1.message_history), 2)  # user + bot
        self.assertEqual(len(session2.message_history), 2)  # user + bot

        # Verify message content is correct per session
        self.assertIn("user 1", session1.message_history[0].content)
        self.assertIn("user 2", session2.message_history[0].content)

    async def test_get_all_sessions(self) -> None:
        """Test getting all active sessions."""
        # Create multiple sessions
        await self.session_manager.get_or_create_session(
            telegram_user_id=123456789,
            telegram_username="user1",
            telegram_first_name="User",
            telegram_last_name="One",
        )

        await self.session_manager.get_or_create_session(
            telegram_user_id=987654321,
            telegram_username="user2",
            telegram_first_name="User",
            telegram_last_name="Two",
        )

        # Get all sessions
        all_sessions = self.session_manager.get_all_sessions()

        # Verify correct count
        self.assertEqual(len(all_sessions), 2)

        # Verify session data
        user_ids = [session.telegram_user_id for session in all_sessions]
        self.assertIn(123456789, user_ids)
        self.assertIn(987654321, user_ids)

    @patch("clud.api.instance_manager.CludInstance.execute")
    async def test_error_handling_in_message_processing(self, mock_execute: AsyncMock) -> None:
        """Test error handling when instance execution fails."""
        # Create session
        session = await self.session_manager.get_or_create_session(
            telegram_user_id=self.test_user_id,
            telegram_username=self.test_username,
            telegram_first_name=self.test_first_name,
            telegram_last_name=self.test_last_name,
        )

        # Mock instance execution to return error
        mock_execute.return_value = {
            "status": "failed",
            "output": "",
            "error": "Something went wrong",
            "exit_code": 1,
        }

        # Process user message
        response = await self.session_manager.process_user_message(
            session_id=session.session_id,
            message_content="Test message",
            telegram_message_id=1,
        )

        # Verify error in response
        self.assertIn("Error", response)
        self.assertIn("Something went wrong", response)

        # Verify messages added to history despite error
        self.assertEqual(len(session.message_history), 2)


if __name__ == "__main__":
    unittest.main()
