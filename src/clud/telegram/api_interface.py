"""Abstract interface for Telegram Bot API.

This module defines the abstract base class and data models for Telegram bot functionality,
allowing for different implementations (real, fake, mock) to be swapped for testing.
"""

from abc import ABC, abstractmethod
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from typing import Any


class MessageSender(Enum):
    """Message sender type."""

    USER = "user"
    BOT = "bot"
    SYSTEM = "system"


@dataclass
class TelegramUser:
    """Abstraction of Telegram user information.

    Attributes:
        id: Telegram user ID
        username: Telegram username (optional)
        first_name: User's first name
        last_name: User's last name (optional)
        is_bot: Whether the user is a bot
    """

    id: int
    username: str | None
    first_name: str
    last_name: str | None = None
    is_bot: bool = False


@dataclass
class TelegramChat:
    """Abstraction of Telegram chat information.

    Attributes:
        id: Telegram chat ID
        type: Chat type (private, group, supergroup, channel)
        title: Chat title (optional, for groups)
        username: Chat username (optional)
    """

    id: int
    type: str
    title: str | None = None
    username: str | None = None


@dataclass
class TelegramMessage:
    """Abstraction of Telegram message information.

    Attributes:
        message_id: Telegram message ID
        from_user: User who sent the message (optional)
        chat: Chat where message was sent
        text: Message text content (optional)
        date: Message timestamp
    """

    message_id: int
    chat: TelegramChat
    from_user: TelegramUser | None = None
    text: str | None = None
    date: datetime | None = None


@dataclass
class TelegramUpdate:
    """Abstraction of Telegram update object.

    Attributes:
        update_id: Update ID
        message: Message object (optional)
        effective_user: User who triggered the update (optional)
        effective_chat: Chat where update occurred (optional)
    """

    update_id: int
    message: TelegramMessage | None = None
    effective_user: TelegramUser | None = None
    effective_chat: TelegramChat | None = None


@dataclass
class MessageResult:
    """Result of sending a message.

    Attributes:
        success: Whether the message was sent successfully
        message_id: ID of the sent message (if successful)
        error: Error message (if failed)
    """

    success: bool
    message_id: int | None = None
    error: str | None = None


@dataclass
class HandlerContext:
    """Context passed to handler functions.

    Attributes:
        bot: Reference to the bot API instance
        user_data: User-specific data storage
        chat_data: Chat-specific data storage
        error: Error object (for error handlers)
    """

    bot: "TelegramBotAPI"
    user_data: dict[str, Any] | None = None
    chat_data: dict[str, Any] | None = None
    error: Exception | None = None


# Type aliases for handler functions
CommandHandler = Callable[[TelegramUpdate, HandlerContext], Awaitable[None]]
MessageHandler = Callable[[TelegramUpdate, HandlerContext], Awaitable[None]]
ErrorHandler = Callable[[TelegramUpdate | None, HandlerContext], Awaitable[None]]


class TelegramBotAPI(ABC):
    """Abstract base class for Telegram Bot API implementations.

    This interface defines all methods required for Telegram bot functionality,
    allowing for different implementations (real, fake, mock) to be swapped.
    """

    @abstractmethod
    async def initialize(self) -> bool:
        """Initialize the bot API.

        Returns:
            True if initialization was successful, False otherwise.
        """
        pass

    @abstractmethod
    async def shutdown(self) -> None:
        """Shutdown the bot API gracefully."""
        pass

    @abstractmethod
    async def send_message(
        self,
        chat_id: str | int,
        text: str,
        parse_mode: str | None = None,
        reply_to_message_id: int | None = None,
    ) -> MessageResult:
        """Send a text message to a chat.

        Args:
            chat_id: Telegram chat ID
            text: Message text to send
            parse_mode: Optional parse mode (e.g., "Markdown", "HTML")
            reply_to_message_id: Optional message ID to reply to

        Returns:
            MessageResult indicating success/failure and message ID
        """
        pass

    @abstractmethod
    async def send_typing_action(self, chat_id: str | int) -> bool:
        """Send typing action to a chat.

        Args:
            chat_id: Telegram chat ID

        Returns:
            True if action was sent successfully, False otherwise
        """
        pass

    @abstractmethod
    async def start_polling(self, drop_pending_updates: bool = True) -> None:
        """Start polling for updates.

        Args:
            drop_pending_updates: Whether to drop pending updates on start
        """
        pass

    @abstractmethod
    async def stop_polling(self) -> None:
        """Stop polling for updates."""
        pass

    @abstractmethod
    def add_command_handler(self, command: str, handler: CommandHandler) -> None:
        """Add a command handler.

        Args:
            command: Command name (without leading slash)
            handler: Async function to handle the command
        """
        pass

    @abstractmethod
    def add_message_handler(self, handler: MessageHandler) -> None:
        """Add a text message handler.

        Args:
            handler: Async function to handle text messages
        """
        pass

    @abstractmethod
    def add_error_handler(self, handler: ErrorHandler) -> None:
        """Add an error handler.

        Args:
            handler: Async function to handle errors
        """
        pass

    @abstractmethod
    async def get_me(self) -> TelegramUser | None:
        """Get information about the bot.

        Returns:
            TelegramUser object representing the bot, or None if failed
        """
        pass


__all__ = [
    "TelegramBotAPI",
    "TelegramUser",
    "TelegramChat",
    "TelegramMessage",
    "TelegramUpdate",
    "MessageResult",
    "HandlerContext",
    "CommandHandler",
    "MessageHandler",
    "ErrorHandler",
    "MessageSender",
]
