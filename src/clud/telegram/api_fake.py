"""Fake implementation of Telegram Bot API for testing.

This module provides a fake in-memory implementation of TelegramBotAPI that simulates
Telegram bot behavior without making any network calls. It's designed for deterministic
testing and includes utilities for simulating incoming messages and inspecting sent messages.
"""

import asyncio
import contextlib
import logging
import random
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any

from .api_config import TelegramAPIConfig
from .api_interface import (
    CommandHandler,
    ErrorHandler,
    HandlerContext,
    MessageHandler,
    MessageResult,
    TelegramBotAPI,
    TelegramChat,
    TelegramMessage,
    TelegramUpdate,
    TelegramUser,
)

logger = logging.getLogger(__name__)


@dataclass
class SentMessage:
    """Record of a sent message in the fake implementation.

    Attributes:
        message_id: ID of the sent message
        chat_id: Chat ID where message was sent
        text: Message text content
        parse_mode: Parse mode used (Markdown, HTML, None)
        reply_to_message_id: Message ID being replied to (optional)
        timestamp: When the message was sent
    """

    message_id: int
    chat_id: int
    text: str
    parse_mode: str | None = None
    reply_to_message_id: int | None = None
    timestamp: datetime = field(default_factory=datetime.now)


def _make_user_data_dict() -> dict[int, dict[str, Any]]:
    """Factory for creating user_data dict with correct type."""
    return {}


def _make_chat_data_dict() -> dict[str, Any]:
    """Factory for creating chat_data dict with correct type."""
    return {}


@dataclass
class ChatState:
    """State tracking for a single chat.

    Attributes:
        chat_id: Chat ID
        typing_sent: Whether typing action was recently sent
        last_message_id: Last message ID in this chat
        user_data: Per-user data storage
        chat_data: Chat-level data storage
    """

    chat_id: int
    typing_sent: bool = False
    last_message_id: int = 0
    user_data: dict[int, dict[str, Any]] = field(default_factory=_make_user_data_dict)
    chat_data: dict[str, Any] = field(default_factory=_make_chat_data_dict)


class FakeTelegramBotAPI(TelegramBotAPI):
    """Fake in-memory implementation of TelegramBotAPI for testing.

    This implementation simulates Telegram bot behavior without network calls:
    - Stores sent messages in memory
    - Simulates incoming messages via test utilities
    - Routes handlers appropriately
    - Supports configurable latency and error injection
    - Thread-safe for concurrent testing

    Example:
        >>> config = TelegramAPIConfig.for_testing(implementation="fake")
        >>> api = FakeTelegramBotAPI(config)
        >>> await api.initialize()
        >>> result = await api.send_message(chat_id=123, text="Hello")
        >>> messages = api.get_sent_messages(chat_id=123)
    """

    def __init__(self, config: TelegramAPIConfig) -> None:
        """Initialize the fake Telegram bot API.

        Args:
            config: Configuration with fake_delay_ms and fake_error_rate settings
        """
        self._config = config
        self._initialized = False
        self._polling = False

        # Message storage
        self._sent_messages: list[SentMessage] = []
        self._next_message_id = 1
        self._message_id_lock = asyncio.Lock()

        # Update queue for simulated incoming messages
        self._pending_updates: asyncio.Queue[TelegramUpdate] = asyncio.Queue()
        self._next_update_id = 1
        self._update_id_lock = asyncio.Lock()

        # Chat state management
        self._chat_states: dict[int, ChatState] = {}
        self._chat_states_lock = asyncio.Lock()

        # Handler storage
        self._command_handlers: dict[str, CommandHandler] = {}
        self._message_handlers: list[MessageHandler] = []
        self._error_handlers: list[ErrorHandler] = []

        # Bot information
        self._bot_user = TelegramUser(
            id=123456789,
            username="test_bot",
            first_name="Test Bot",
            is_bot=True,
        )

        # Polling task
        self._polling_task: asyncio.Task[None] | None = None

        # Error injection
        self._error_rate = config.fake_error_rate
        self._delay_ms = config.fake_delay_ms

    async def initialize(self) -> bool:
        """Initialize the fake bot API.

        Returns:
            True (always successful for fake implementation)
        """
        if self._initialized:
            logger.warning("FakeTelegramBotAPI already initialized")
            return True

        logger.info("Initializing FakeTelegramBotAPI")
        self._initialized = True
        return True

    async def shutdown(self) -> None:
        """Shutdown the fake bot API gracefully."""
        if not self._initialized:
            return

        logger.info("Shutting down FakeTelegramBotAPI")

        # Stop polling if active
        if self._polling:
            await self.stop_polling()

        self._initialized = False

    async def send_message(
        self,
        chat_id: str | int,
        text: str,
        parse_mode: str | None = None,
        reply_to_message_id: int | None = None,
    ) -> MessageResult:
        """Send a text message to a chat (simulated).

        Args:
            chat_id: Telegram chat ID
            text: Message text to send
            parse_mode: Optional parse mode (e.g., "Markdown", "HTML")
            reply_to_message_id: Optional message ID to reply to

        Returns:
            MessageResult indicating success/failure and message ID
        """
        if not self._initialized:
            return MessageResult(
                success=False,
                error="Bot not initialized",
            )

        # Simulate network delay
        if self._delay_ms > 0:
            await asyncio.sleep(self._delay_ms / 1000.0)

        # Simulate errors based on error rate
        if self._error_rate > 0 and random.random() < self._error_rate:
            error_msg = "Simulated network error"
            logger.error(f"Fake API error: {error_msg}")
            return MessageResult(
                success=False,
                error=error_msg,
            )

        # Convert chat_id to int
        chat_id_int = int(chat_id)

        # Generate message ID
        async with self._message_id_lock:
            message_id = self._next_message_id
            self._next_message_id += 1

        # Store sent message
        sent_message = SentMessage(
            message_id=message_id,
            chat_id=chat_id_int,
            text=text,
            parse_mode=parse_mode,
            reply_to_message_id=reply_to_message_id,
        )
        self._sent_messages.append(sent_message)

        # Update chat state
        async with self._chat_states_lock:
            if chat_id_int not in self._chat_states:
                self._chat_states[chat_id_int] = ChatState(chat_id=chat_id_int)
            self._chat_states[chat_id_int].last_message_id = message_id

        logger.debug(f"Fake API sent message {message_id} to chat {chat_id_int}: {text[:50]}...")
        return MessageResult(success=True, message_id=message_id)

    async def send_typing_action(self, chat_id: str | int) -> bool:
        """Send typing action to a chat (simulated).

        Args:
            chat_id: Telegram chat ID

        Returns:
            True if action was sent successfully
        """
        if not self._initialized:
            return False

        # Simulate network delay
        if self._delay_ms > 0:
            await asyncio.sleep(self._delay_ms / 1000.0)

        # Simulate errors based on error rate
        if self._error_rate > 0 and random.random() < self._error_rate:
            logger.error("Fake API error: Simulated typing action error")
            return False

        # Convert chat_id to int
        chat_id_int = int(chat_id)

        # Update chat state
        async with self._chat_states_lock:
            if chat_id_int not in self._chat_states:
                self._chat_states[chat_id_int] = ChatState(chat_id=chat_id_int)
            self._chat_states[chat_id_int].typing_sent = True

        logger.debug(f"Fake API sent typing action to chat {chat_id_int}")
        return True

    async def start_polling(self, drop_pending_updates: bool = True) -> None:
        """Start polling for updates (simulated).

        Args:
            drop_pending_updates: Whether to drop pending updates on start
        """
        if not self._initialized:
            logger.error("Cannot start polling: bot not initialized")
            return

        if self._polling:
            logger.warning("Polling already started")
            return

        logger.info("Starting fake polling")
        self._polling = True

        # Clear pending updates if requested
        if drop_pending_updates:
            while not self._pending_updates.empty():
                try:
                    self._pending_updates.get_nowait()
                except asyncio.QueueEmpty:
                    break

        # Start polling task
        self._polling_task = asyncio.create_task(self._poll_updates())

    async def stop_polling(self) -> None:
        """Stop polling for updates."""
        if not self._polling:
            return

        logger.info("Stopping fake polling")
        self._polling = False

        # Cancel polling task
        if self._polling_task:
            self._polling_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._polling_task
            self._polling_task = None

    async def _poll_updates(self) -> None:
        """Poll for updates from the queue (internal method)."""
        try:
            while self._polling:
                try:
                    # Wait for update with timeout
                    update = await asyncio.wait_for(
                        self._pending_updates.get(),
                        timeout=1.0,
                    )

                    # Process the update
                    await self._process_update(update)

                except TimeoutError:
                    # No updates, continue polling
                    continue
                except asyncio.CancelledError:
                    raise
                except Exception as e:
                    logger.error(f"Error processing update: {e}")
                    # Call error handlers
                    await self._handle_error(None, e)

        except asyncio.CancelledError:
            logger.debug("Polling task cancelled")
            raise

    async def _process_update(self, update: TelegramUpdate) -> None:
        """Process an incoming update by routing to handlers.

        Args:
            update: Update to process
        """
        try:
            # Extract message text
            if not update.message or not update.message.text:
                return

            text = update.message.text
            chat_id = update.message.chat.id
            user_id = update.effective_user.id if update.effective_user else 0

            # Get or create chat state
            async with self._chat_states_lock:
                if chat_id not in self._chat_states:
                    self._chat_states[chat_id] = ChatState(chat_id=chat_id)
                chat_state = self._chat_states[chat_id]

                # Get user data and chat data
                if user_id not in chat_state.user_data:
                    chat_state.user_data[user_id] = {}
                user_data = chat_state.user_data[user_id]
                chat_data = chat_state.chat_data

            # Create handler context
            context = HandlerContext(
                bot=self,
                user_data=user_data,
                chat_data=chat_data,
            )

            # Check if it's a command
            if text.startswith("/"):
                command = text[1:].split()[0]
                if command in self._command_handlers:
                    handler = self._command_handlers[command]
                    await handler(update, context)
                    return

            # Call message handlers
            for handler in self._message_handlers:
                await handler(update, context)

        except Exception as e:
            logger.error(f"Error in _process_update: {e}")
            await self._handle_error(update, e)

    async def _handle_error(self, update: TelegramUpdate | None, error: Exception) -> None:
        """Handle errors by calling error handlers.

        Args:
            update: Update that caused the error (optional)
            error: Exception that occurred
        """
        context = HandlerContext(bot=self, error=error)
        for handler in self._error_handlers:
            try:
                await handler(update, context)
            except Exception as e:
                logger.error(f"Error in error handler: {e}")

    def add_command_handler(self, command: str, handler: CommandHandler) -> None:
        """Add a command handler.

        Args:
            command: Command name (without leading slash)
            handler: Async function to handle the command
        """
        self._command_handlers[command] = handler
        logger.debug(f"Registered command handler: /{command}")

    def add_message_handler(self, handler: MessageHandler) -> None:
        """Add a text message handler.

        Args:
            handler: Async function to handle text messages
        """
        self._message_handlers.append(handler)
        logger.debug("Registered message handler")

    def add_error_handler(self, handler: ErrorHandler) -> None:
        """Add an error handler.

        Args:
            handler: Async function to handle errors
        """
        self._error_handlers.append(handler)
        logger.debug("Registered error handler")

    async def get_me(self) -> TelegramUser | None:
        """Get information about the bot.

        Returns:
            TelegramUser object representing the bot
        """
        if not self._initialized:
            return None

        # Simulate network delay
        if self._delay_ms > 0:
            await asyncio.sleep(self._delay_ms / 1000.0)

        return self._bot_user

    # Test utility methods

    async def simulate_incoming_message(
        self,
        chat_id: int,
        text: str,
        user: TelegramUser,
        message_id: int | None = None,
    ) -> None:
        """Simulate an incoming message from a user (test utility).

        Args:
            chat_id: Chat ID where message is received
            text: Message text content
            user: User who sent the message
            message_id: Optional message ID (auto-generated if not provided)
        """
        # Generate update ID
        async with self._update_id_lock:
            update_id = self._next_update_id
            self._next_update_id += 1

        # Generate message ID if not provided
        if message_id is None:
            async with self._message_id_lock:
                message_id = self._next_message_id
                self._next_message_id += 1

        # Create update
        chat = TelegramChat(id=chat_id, type="private", username=user.username)
        message = TelegramMessage(
            message_id=message_id,
            chat=chat,
            from_user=user,
            text=text,
            date=datetime.now(),
        )
        update = TelegramUpdate(
            update_id=update_id,
            message=message,
            effective_user=user,
            effective_chat=chat,
        )

        # Queue the update
        await self._pending_updates.put(update)
        logger.debug(f"Simulated incoming message from user {user.id}: {text}")

    async def simulate_command(
        self,
        chat_id: int,
        command: str,
        user: TelegramUser,
    ) -> None:
        """Simulate an incoming command from a user (test utility).

        Args:
            chat_id: Chat ID where command is received
            command: Command text (with or without leading slash)
            user: User who sent the command
        """
        # Ensure command has leading slash
        if not command.startswith("/"):
            command = f"/{command}"

        await self.simulate_incoming_message(chat_id, command, user)
        logger.debug(f"Simulated command from user {user.id}: {command}")

    def get_sent_messages(self, chat_id: int | None = None) -> list[SentMessage]:
        """Get sent messages, optionally filtered by chat ID (test utility).

        Args:
            chat_id: Optional chat ID to filter by

        Returns:
            List of sent messages
        """
        if chat_id is None:
            return self._sent_messages.copy()
        return [msg for msg in self._sent_messages if msg.chat_id == chat_id]

    def get_last_sent_message(self, chat_id: int) -> SentMessage | None:
        """Get the last message sent to a specific chat (test utility).

        Args:
            chat_id: Chat ID to check

        Returns:
            Last sent message or None if no messages sent
        """
        messages = self.get_sent_messages(chat_id)
        return messages[-1] if messages else None

    def clear_history(self) -> None:
        """Clear all message history and state (test utility)."""
        self._sent_messages.clear()
        self._chat_states.clear()
        logger.debug("Cleared fake API history")

    def set_error_rate(self, rate: float) -> None:
        """Set error injection rate for testing (test utility).

        Args:
            rate: Error rate between 0.0 and 1.0
        """
        if not 0.0 <= rate <= 1.0:
            msg = f"Error rate must be between 0.0 and 1.0, got {rate}"
            raise ValueError(msg)
        self._error_rate = rate
        logger.debug(f"Set fake API error rate to {rate}")

    def was_typing_sent(self, chat_id: int) -> bool:
        """Check if typing action was sent to a chat (test utility).

        Args:
            chat_id: Chat ID to check

        Returns:
            True if typing action was sent
        """
        return chat_id in self._chat_states and self._chat_states[chat_id].typing_sent

    def get_handler_count(self, handler_type: str) -> int:
        """Get count of registered handlers by type (test utility).

        Args:
            handler_type: Type of handler ("command", "message", "error")

        Returns:
            Number of registered handlers
        """
        if handler_type == "command":
            return len(self._command_handlers)
        elif handler_type == "message":
            return len(self._message_handlers)
        elif handler_type == "error":
            return len(self._error_handlers)
        else:
            msg = f"Unknown handler type: {handler_type}"
            raise ValueError(msg)


__all__ = ["FakeTelegramBotAPI", "SentMessage", "ChatState"]
