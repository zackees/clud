"""Unit tests for FakeTelegramBotAPI implementation.

Tests the fake in-memory Telegram API implementation for:
- Message sending and retrieval
- Incoming message simulation
- Handler registration and routing
- Error injection
- Latency simulation
- State management
- Thread safety
"""

import asyncio
import unittest

from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_fake import FakeTelegramBotAPI
from clud.telegram.api_interface import HandlerContext, TelegramUpdate, TelegramUser


class TestFakeTelegramBotAPI(unittest.IsolatedAsyncioTestCase):
    """Test FakeTelegramBotAPI implementation."""

    async def asyncSetUp(self) -> None:
        """Set up test fixtures."""
        self.config = TelegramAPIConfig.for_testing(implementation="fake")
        self.api = FakeTelegramBotAPI(config=self.config)
        await self.api.initialize()

    async def asyncTearDown(self) -> None:
        """Clean up after tests."""
        if self.api._polling:
            await self.api.stop_polling()
        await self.api.shutdown()

    async def test_initialize(self) -> None:
        """Test API initialization."""
        result = await self.api.initialize()
        self.assertTrue(result)

    async def test_get_me(self) -> None:
        """Test getting bot information."""
        bot_user = await self.api.get_me()
        self.assertIsNotNone(bot_user)
        self.assertEqual(bot_user.username, "test_bot")
        self.assertEqual(bot_user.id, 123456789)
        self.assertTrue(bot_user.is_bot)

    async def test_send_message_success(self) -> None:
        """Test sending a message successfully."""
        result = await self.api.send_message(chat_id=123, text="Hello, World!")

        self.assertTrue(result.success)
        self.assertIsNotNone(result.message_id)
        self.assertEqual(result.message_id, 1)
        self.assertIsNone(result.error)

    async def test_send_message_with_parse_mode(self) -> None:
        """Test sending a message with parse mode."""
        result = await self.api.send_message(
            chat_id=123,
            text="**Bold** text",
            parse_mode="Markdown",
        )

        self.assertTrue(result.success)
        self.assertIsNotNone(result.message_id)

    async def test_send_message_with_reply(self) -> None:
        """Test sending a message as a reply."""
        result = await self.api.send_message(
            chat_id=123,
            text="Reply text",
            reply_to_message_id=42,
        )

        self.assertTrue(result.success)
        self.assertIsNotNone(result.message_id)

    async def test_send_typing_action_success(self) -> None:
        """Test sending typing action successfully."""
        result = await self.api.send_typing_action(chat_id=123)
        self.assertTrue(result)

    async def test_get_sent_messages_all(self) -> None:
        """Test retrieving all sent messages."""
        await self.api.send_message(chat_id=123, text="Message 1")
        await self.api.send_message(chat_id=456, text="Message 2")
        await self.api.send_message(chat_id=123, text="Message 3")

        messages = self.api.get_sent_messages()
        self.assertEqual(len(messages), 3)
        self.assertEqual(messages[0].text, "Message 1")
        self.assertEqual(messages[1].text, "Message 2")
        self.assertEqual(messages[2].text, "Message 3")

    async def test_get_sent_messages_filtered_by_chat(self) -> None:
        """Test retrieving sent messages filtered by chat_id."""
        await self.api.send_message(chat_id=123, text="Message 1")
        await self.api.send_message(chat_id=456, text="Message 2")
        await self.api.send_message(chat_id=123, text="Message 3")

        messages = self.api.get_sent_messages(chat_id=123)
        self.assertEqual(len(messages), 2)
        self.assertEqual(messages[0].text, "Message 1")
        self.assertEqual(messages[1].text, "Message 3")

    async def test_get_last_sent_message(self) -> None:
        """Test getting the last sent message for a chat."""
        await self.api.send_message(chat_id=123, text="Message 1")
        await self.api.send_message(chat_id=123, text="Message 2")

        last_message = self.api.get_last_sent_message(chat_id=123)
        self.assertIsNotNone(last_message)
        self.assertEqual(last_message.text, "Message 2")

    async def test_get_last_sent_message_no_messages(self) -> None:
        """Test getting last sent message when no messages exist."""
        last_message = self.api.get_last_sent_message(chat_id=999)
        self.assertIsNone(last_message)

    async def test_clear_history(self) -> None:
        """Test clearing message history."""
        await self.api.send_message(chat_id=123, text="Message 1")
        await self.api.send_message(chat_id=456, text="Message 2")

        self.assertEqual(len(self.api.get_sent_messages()), 2)

        self.api.clear_history()

        self.assertEqual(len(self.api.get_sent_messages()), 0)

    async def test_message_id_increments(self) -> None:
        """Test that message IDs increment correctly."""
        result1 = await self.api.send_message(chat_id=123, text="Message 1")
        result2 = await self.api.send_message(chat_id=123, text="Message 2")
        result3 = await self.api.send_message(chat_id=123, text="Message 3")

        self.assertEqual(result1.message_id, 1)
        self.assertEqual(result2.message_id, 2)
        self.assertEqual(result3.message_id, 3)

    async def test_was_typing_sent(self) -> None:
        """Test checking if typing action was sent."""
        self.assertFalse(self.api.was_typing_sent(chat_id=123))

        await self.api.send_typing_action(chat_id=123)

        self.assertTrue(self.api.was_typing_sent(chat_id=123))

    async def test_add_command_handler(self) -> None:
        """Test registering a command handler."""

        async def handler(update: TelegramUpdate, context: HandlerContext) -> None:
            pass

        self.api.add_command_handler("start", handler)

        self.assertEqual(self.api.get_handler_count("command"), 1)

    async def test_add_message_handler(self) -> None:
        """Test registering a message handler."""

        async def handler(update: TelegramUpdate, context: HandlerContext) -> None:
            pass

        self.api.add_message_handler(handler)

        self.assertEqual(self.api.get_handler_count("message"), 1)

    async def test_add_error_handler(self) -> None:
        """Test registering an error handler."""

        async def handler(update: TelegramUpdate | None, context: HandlerContext) -> None:
            pass

        self.api.add_error_handler(handler)

        self.assertEqual(self.api.get_handler_count("error"), 1)

    async def test_command_handler_routing(self) -> None:
        """Test that command handlers are called correctly."""
        called = asyncio.Event()
        received_command = None

        async def start_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            nonlocal received_command
            received_command = update.message.text
            called.set()

        self.api.add_command_handler("start", start_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )
        await self.api.simulate_command(chat_id=123, command="/start", user=user)

        # Wait for handler to be called
        await asyncio.wait_for(called.wait(), timeout=1.0)

        self.assertEqual(received_command, "/start")

    async def test_message_handler_routing(self) -> None:
        """Test that message handlers are called correctly."""
        called = asyncio.Event()
        received_text = None

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            nonlocal received_text
            received_text = update.message.text
            called.set()

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )
        await self.api.simulate_incoming_message(
            chat_id=123,
            text="Hello from user",
            user=user,
        )

        # Wait for handler to be called
        await asyncio.wait_for(called.wait(), timeout=1.0)

        self.assertEqual(received_text, "Hello from user")

    async def test_simulate_incoming_message(self) -> None:
        """Test simulating incoming messages."""
        received_messages: list[str] = []

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            if update.message.text:
                received_messages.append(update.message.text)

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )

        await self.api.simulate_incoming_message(chat_id=123, text="Message 1", user=user)
        await self.api.simulate_incoming_message(chat_id=123, text="Message 2", user=user)
        await self.api.simulate_incoming_message(chat_id=123, text="Message 3", user=user)

        # Wait for processing
        await asyncio.sleep(0.2)

        self.assertEqual(len(received_messages), 3)
        self.assertEqual(received_messages[0], "Message 1")
        self.assertEqual(received_messages[1], "Message 2")
        self.assertEqual(received_messages[2], "Message 3")

    async def test_simulate_command(self) -> None:
        """Test simulating commands."""
        received_commands: list[str] = []

        async def start_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            if update.message.text:
                received_commands.append(update.message.text)

        async def help_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            if update.message.text:
                received_commands.append(update.message.text)

        self.api.add_command_handler("start", start_handler)
        self.api.add_command_handler("help", help_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )

        await self.api.simulate_command(chat_id=123, command="/start", user=user)
        await self.api.simulate_command(chat_id=123, command="/help", user=user)

        # Wait for processing
        await asyncio.sleep(0.2)

        self.assertEqual(len(received_commands), 2)
        self.assertEqual(received_commands[0], "/start")
        self.assertEqual(received_commands[1], "/help")

    async def test_simulate_command_without_slash(self) -> None:
        """Test that simulate_command adds leading slash if missing."""
        received_commands: list[str] = []

        async def start_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            if update.message.text:
                received_commands.append(update.message.text)

        self.api.add_command_handler("start", start_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )

        # Command without leading slash
        await self.api.simulate_command(chat_id=123, command="start", user=user)

        # Wait for processing
        await asyncio.sleep(0.2)

        self.assertEqual(len(received_commands), 1)
        self.assertEqual(received_commands[0], "/start")

    async def test_handler_context_has_bot_reference(self) -> None:
        """Test that handler context includes bot reference."""
        received_bot = None

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            nonlocal received_bot
            received_bot = context.bot

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )
        await self.api.simulate_incoming_message(chat_id=123, text="Test", user=user)

        # Wait for processing
        await asyncio.sleep(0.2)

        self.assertIs(received_bot, self.api)

    async def test_handler_context_has_user_data(self) -> None:
        """Test that handler context includes user_data."""

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            context.user_data["counter"] = context.user_data.get("counter", 0) + 1

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )

        # Send multiple messages from the same user
        await self.api.simulate_incoming_message(chat_id=123, text="Message 1", user=user)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=123, text="Message 2", user=user)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=123, text="Message 3", user=user)
        await asyncio.sleep(0.1)

        # Get the user data
        chat_state = self.api._chat_states.get(123)
        self.assertIsNotNone(chat_state)
        user_data = chat_state.user_data.get(456)
        self.assertIsNotNone(user_data)
        self.assertEqual(user_data.get("counter"), 3)

    async def test_handler_context_has_chat_data(self) -> None:
        """Test that handler context includes chat_data."""

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            context.chat_data["message_count"] = context.chat_data.get("message_count", 0) + 1

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user1 = TelegramUser(id=456, username="user1", first_name="User", last_name="One", is_bot=False)
        user2 = TelegramUser(id=789, username="user2", first_name="User", last_name="Two", is_bot=False)

        # Send messages from different users to the same chat
        await self.api.simulate_incoming_message(chat_id=123, text="Message 1", user=user1)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=123, text="Message 2", user=user2)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=123, text="Message 3", user=user1)
        await asyncio.sleep(0.1)

        # Get the chat data
        chat_state = self.api._chat_states.get(123)
        self.assertIsNotNone(chat_state)
        self.assertEqual(chat_state.chat_data.get("message_count"), 3)

    async def test_error_handler_called_on_exception(self) -> None:
        """Test that error handlers are called when handlers raise exceptions."""
        error_caught = asyncio.Event()
        caught_error: Exception | None = None

        async def failing_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            raise ValueError("Test error")

        async def error_handler(update: TelegramUpdate | None, context: HandlerContext) -> None:
            nonlocal caught_error
            caught_error = context.error
            error_caught.set()

        self.api.add_message_handler(failing_handler)
        self.api.add_error_handler(error_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )
        await self.api.simulate_incoming_message(chat_id=123, text="Test", user=user)

        # Wait for error handler to be called
        await asyncio.wait_for(error_caught.wait(), timeout=1.0)

        self.assertIsNotNone(caught_error)
        self.assertIsInstance(caught_error, ValueError)
        self.assertEqual(str(caught_error), "Test error")

    async def test_start_stop_polling(self) -> None:
        """Test starting and stopping polling."""
        self.assertFalse(self.api._polling)

        await self.api.start_polling()
        self.assertTrue(self.api._polling)

        await self.api.stop_polling()
        self.assertFalse(self.api._polling)

    async def test_stop_polling_cancels_task(self) -> None:
        """Test that stop_polling cancels the polling task."""
        await self.api.start_polling()
        polling_task = self.api._polling_task

        self.assertIsNotNone(polling_task)
        self.assertFalse(polling_task.done())

        await self.api.stop_polling()

        # Wait a bit for task to be cancelled
        await asyncio.sleep(0.1)

        self.assertTrue(polling_task.done())
        self.assertTrue(polling_task.cancelled())

    async def test_shutdown_stops_polling(self) -> None:
        """Test that shutdown stops polling if active."""
        await self.api.start_polling()
        self.assertTrue(self.api._polling)

        await self.api.shutdown()

        self.assertFalse(self.api._polling)


class TestFakeTelegramBotAPIErrorInjection(unittest.IsolatedAsyncioTestCase):
    """Test error injection functionality."""

    async def asyncSetUp(self) -> None:
        """Set up test fixtures with error injection."""
        self.config = TelegramAPIConfig(
            implementation="fake",
            bot_token=None,
            fake_delay_ms=0,
            fake_error_rate=1.0,  # 100% error rate
        )
        self.api = FakeTelegramBotAPI(config=self.config)
        await self.api.initialize()

    async def asyncTearDown(self) -> None:
        """Clean up after tests."""
        if self.api._polling:
            await self.api.stop_polling()
        await self.api.shutdown()

    async def test_send_message_fails_with_error_injection(self) -> None:
        """Test that send_message fails when error rate is 1.0."""
        result = await self.api.send_message(chat_id=123, text="Test")

        self.assertFalse(result.success)
        self.assertIsNone(result.message_id)
        self.assertIsNotNone(result.error)
        # Type narrow: we just asserted error is not None
        error_msg: str = result.error  # type: ignore[assignment]
        self.assertIn("Simulated", error_msg)

    async def test_send_typing_action_fails_with_error_injection(self) -> None:
        """Test that send_typing_action fails when error rate is 1.0."""
        result = await self.api.send_typing_action(chat_id=123)

        self.assertFalse(result)

    async def test_set_error_rate(self) -> None:
        """Test changing error rate dynamically."""
        # Initially 100% errors
        result = await self.api.send_message(chat_id=123, text="Test")
        self.assertFalse(result.success)

        # Change to 0% errors
        self.api.set_error_rate(0.0)
        result = await self.api.send_message(chat_id=123, text="Test")
        self.assertTrue(result.success)

        # Change back to 100% errors
        self.api.set_error_rate(1.0)
        result = await self.api.send_message(chat_id=123, text="Test")
        self.assertFalse(result.success)

    async def test_set_error_rate_validates_range(self) -> None:
        """Test that set_error_rate validates the range."""
        with self.assertRaises(ValueError):
            self.api.set_error_rate(-0.1)

        with self.assertRaises(ValueError):
            self.api.set_error_rate(1.1)

        # Valid values should work
        self.api.set_error_rate(0.0)
        self.api.set_error_rate(0.5)
        self.api.set_error_rate(1.0)


class TestFakeTelegramBotAPILatency(unittest.IsolatedAsyncioTestCase):
    """Test latency simulation functionality."""

    async def asyncSetUp(self) -> None:
        """Set up test fixtures with latency."""
        self.config = TelegramAPIConfig(
            implementation="fake",
            bot_token=None,
            fake_delay_ms=50,  # 50ms delay
            fake_error_rate=0.0,
        )
        self.api = FakeTelegramBotAPI(config=self.config)
        await self.api.initialize()

    async def asyncTearDown(self) -> None:
        """Clean up after tests."""
        if self.api._polling:
            await self.api.stop_polling()
        await self.api.shutdown()

    async def test_send_message_respects_latency(self) -> None:
        """Test that send_message respects configured latency."""
        import time

        start = time.perf_counter()
        await self.api.send_message(chat_id=123, text="Test")
        elapsed = (time.perf_counter() - start) * 1000  # Convert to ms

        # Should take at least 50ms
        self.assertGreaterEqual(elapsed, 45)  # Small tolerance

    async def test_send_typing_action_respects_latency(self) -> None:
        """Test that send_typing_action respects configured latency."""
        import time

        start = time.perf_counter()
        await self.api.send_typing_action(chat_id=123)
        elapsed = (time.perf_counter() - start) * 1000  # Convert to ms

        # Should take at least 50ms
        self.assertGreaterEqual(elapsed, 45)  # Small tolerance


class TestFakeTelegramBotAPIThreadSafety(unittest.IsolatedAsyncioTestCase):
    """Test thread-safety of concurrent operations."""

    async def asyncSetUp(self) -> None:
        """Set up test fixtures."""
        self.config = TelegramAPIConfig.for_testing(implementation="fake")
        self.api = FakeTelegramBotAPI(config=self.config)
        await self.api.initialize()

    async def asyncTearDown(self) -> None:
        """Clean up after tests."""
        if self.api._polling:
            await self.api.stop_polling()
        await self.api.shutdown()

    async def test_concurrent_message_sending(self) -> None:
        """Test sending messages concurrently from multiple tasks."""

        async def send_messages(chat_id: int, count: int) -> None:
            for i in range(count):
                await self.api.send_message(chat_id=chat_id, text=f"Message {i}")

        # Send messages concurrently from multiple tasks
        tasks = [
            asyncio.create_task(send_messages(123, 10)),
            asyncio.create_task(send_messages(456, 10)),
            asyncio.create_task(send_messages(789, 10)),
        ]

        await asyncio.gather(*tasks)

        # Should have 30 total messages
        all_messages = self.api.get_sent_messages()
        self.assertEqual(len(all_messages), 30)

        # Each chat should have 10 messages
        self.assertEqual(len(self.api.get_sent_messages(chat_id=123)), 10)
        self.assertEqual(len(self.api.get_sent_messages(chat_id=456)), 10)
        self.assertEqual(len(self.api.get_sent_messages(chat_id=789)), 10)

    async def test_concurrent_message_id_generation(self) -> None:
        """Test that message ID generation is thread-safe."""

        async def send_message() -> int | None:
            result = await self.api.send_message(chat_id=123, text="Test")
            return result.message_id

        # Send many messages concurrently
        tasks = [asyncio.create_task(send_message()) for _ in range(100)]
        message_ids = await asyncio.gather(*tasks)

        # All IDs should be unique
        self.assertEqual(len(set(message_ids)), 100)

        # IDs should be in range 1-100
        self.assertEqual(set(message_ids), set(range(1, 101)))

    async def test_concurrent_simulate_and_process(self) -> None:
        """Test concurrent message simulation and processing."""
        received_messages: list[str] = []
        lock = asyncio.Lock()

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            if update.message.text:
                async with lock:
                    received_messages.append(update.message.text)

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user = TelegramUser(
            id=456,
            username="testuser",
            first_name="Test",
            last_name="User",
            is_bot=False,
        )

        # Simulate many messages concurrently
        tasks = [
            asyncio.create_task(
                self.api.simulate_incoming_message(
                    chat_id=123,
                    text=f"Message {i}",
                    user=user,
                )
            )
            for i in range(50)
        ]

        await asyncio.gather(*tasks)

        # Wait for all messages to be processed
        await asyncio.sleep(0.5)

        # Should have received all 50 messages
        self.assertEqual(len(received_messages), 50)


class TestFakeTelegramBotAPIChatStateManagement(unittest.IsolatedAsyncioTestCase):
    """Test chat state management functionality."""

    async def asyncSetUp(self) -> None:
        """Set up test fixtures."""
        self.config = TelegramAPIConfig.for_testing(implementation="fake")
        self.api = FakeTelegramBotAPI(config=self.config)
        await self.api.initialize()

    async def asyncTearDown(self) -> None:
        """Clean up after tests."""
        if self.api._polling:
            await self.api.stop_polling()
        await self.api.shutdown()

    async def test_chat_state_created_on_first_message(self) -> None:
        """Test that chat state is created when first message is received."""
        self.assertNotIn(123, self.api._chat_states)

        await self.api.start_polling()
        user = TelegramUser(id=456, username="test", first_name="Test", last_name="User", is_bot=False)
        await self.api.simulate_incoming_message(chat_id=123, text="Hello", user=user)

        # Wait for processing
        await asyncio.sleep(0.1)

        # Check that chat state was created (accessing internal state for testing)
        self.assertIn(123, self.api._chat_states)

    async def test_multiple_chats_have_separate_states(self) -> None:
        """Test that different chats have separate states."""

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            context.chat_data["count"] = context.chat_data.get("count", 0) + 1

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user = TelegramUser(id=456, username="test", first_name="Test", last_name="User", is_bot=False)

        # Send messages to different chats
        await self.api.simulate_incoming_message(chat_id=123, text="Hello", user=user)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=456, text="Hello", user=user)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=123, text="Hello", user=user)
        await asyncio.sleep(0.1)

        # Chat 123 should have count of 2
        chat_state_123 = self.api._chat_states.get(123)
        self.assertEqual(chat_state_123.chat_data.get("count"), 2)

        # Chat 456 should have count of 1
        chat_state_456 = self.api._chat_states.get(456)
        self.assertEqual(chat_state_456.chat_data.get("count"), 1)

    async def test_user_data_separate_per_user(self) -> None:
        """Test that different users have separate user_data."""

        async def message_handler(update: TelegramUpdate, context: HandlerContext) -> None:
            context.user_data["message_count"] = context.user_data.get("message_count", 0) + 1

        self.api.add_message_handler(message_handler)
        await self.api.start_polling()

        user1 = TelegramUser(id=456, username="user1", first_name="User", last_name="One", is_bot=False)
        user2 = TelegramUser(id=789, username="user2", first_name="User", last_name="Two", is_bot=False)

        # Send messages from different users
        await self.api.simulate_incoming_message(chat_id=123, text="Hello", user=user1)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=123, text="Hello", user=user2)
        await asyncio.sleep(0.1)
        await self.api.simulate_incoming_message(chat_id=123, text="Hello", user=user1)
        await asyncio.sleep(0.1)

        # User 1 should have count of 2
        chat_state = self.api._chat_states.get(123)
        user1_data = chat_state.user_data.get(456)
        self.assertEqual(user1_data.get("message_count"), 2)

        # User 2 should have count of 1
        user2_data = chat_state.user_data.get(789)
        self.assertEqual(user2_data.get("message_count"), 1)

    async def test_clear_history_clears_chat_states(self) -> None:
        """Test that clear_history clears chat states."""
        await self.api.start_polling()
        user = TelegramUser(id=456, username="test", first_name="Test", last_name="User", is_bot=False)

        await self.api.simulate_incoming_message(chat_id=123, text="Hello", user=user)
        await asyncio.sleep(0.1)

        self.assertIn(123, self.api._chat_states)

        self.api.clear_history()

        self.assertEqual(len(self.api._chat_states), 0)


if __name__ == "__main__":
    unittest.main()
