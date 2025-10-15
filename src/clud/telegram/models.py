"""Data models for Telegram integration.

This module defines the core data structures for managing Telegram bot sessions,
messages, and WebSocket events.
"""

import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any


class ContentType(str, Enum):
    """Type of message content."""

    TEXT = "text"
    MARKDOWN = "markdown"
    CODE = "code"
    IMAGE = "image"
    FILE = "file"


class MessageSender(str, Enum):
    """Sender of a message."""

    USER = "user"
    BOT = "bot"
    WEB = "web"


class EventType(str, Enum):
    """WebSocket event types."""

    # Client -> Server
    SUBSCRIBE = "subscribe"
    SEND_MESSAGE = "send_message"
    UNSUBSCRIBE = "unsubscribe"

    # Server -> Client
    MESSAGE = "message"
    TYPING = "typing"
    SESSION_UPDATE = "session_update"
    HISTORY = "history"
    ERROR = "error"
    CONNECTED = "connected"
    AUTH_SUCCESS = "auth_success"


@dataclass
class TelegramMessage:
    """A message in a Telegram conversation.

    Represents a single message exchanged between a Telegram user and the bot,
    with support for various content types and metadata.
    """

    message_id: str
    session_id: str
    telegram_message_id: int
    sender: MessageSender
    content: str
    content_type: ContentType = ContentType.TEXT
    timestamp: datetime = field(default_factory=datetime.now)
    metadata: dict[str, Any] = field(default_factory=dict)  # pyright: ignore[reportUnknownVariableType]

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization.

        Returns:
            Dictionary representation of the message
        """
        return {
            "message_id": self.message_id,
            "session_id": self.session_id,
            "telegram_message_id": self.telegram_message_id,
            "sender": self.sender.value,
            "content": self.content,
            "content_type": self.content_type.value,
            "timestamp": self.timestamp.isoformat(),
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "TelegramMessage":
        """Create from dictionary.

        Args:
            data: Dictionary containing message data

        Returns:
            TelegramMessage instance
        """
        return cls(
            message_id=data["message_id"],
            session_id=data["session_id"],
            telegram_message_id=data["telegram_message_id"],
            sender=MessageSender(data["sender"]),
            content=data["content"],
            content_type=ContentType(data["content_type"]),
            timestamp=datetime.fromisoformat(data["timestamp"]),
            metadata=data.get("metadata", {}),
        )

    @classmethod
    def create_user_message(cls, session_id: str, telegram_message_id: int, content: str, metadata: dict[str, Any] | None = None) -> "TelegramMessage":
        """Create a user message.

        Args:
            session_id: Session identifier
            telegram_message_id: Telegram's message ID
            content: Message content
            metadata: Optional metadata

        Returns:
            New TelegramMessage instance
        """
        return cls(
            message_id=str(uuid.uuid4()),
            session_id=session_id,
            telegram_message_id=telegram_message_id,
            sender=MessageSender.USER,
            content=content,
            metadata=metadata or {},
        )

    @classmethod
    def create_bot_message(cls, session_id: str, content: str, metadata: dict[str, Any] | None = None) -> "TelegramMessage":
        """Create a bot message.

        Args:
            session_id: Session identifier
            content: Message content
            metadata: Optional metadata

        Returns:
            New TelegramMessage instance
        """
        return cls(
            message_id=str(uuid.uuid4()),
            session_id=session_id,
            telegram_message_id=0,  # Bot messages don't have Telegram message IDs initially
            sender=MessageSender.BOT,
            content=content,
            metadata=metadata or {},
        )


@dataclass
class TelegramSession:
    """A Telegram bot session.

    Represents an ongoing conversation between a Telegram user and the bot,
    including message history and metadata.
    """

    session_id: str
    telegram_user_id: int
    telegram_username: str
    telegram_first_name: str
    telegram_last_name: str | None = None
    instance_id: str | None = None
    message_history: list[TelegramMessage] = field(default_factory=list)  # pyright: ignore[reportUnknownVariableType]
    created_at: datetime = field(default_factory=datetime.now)
    last_activity: datetime = field(default_factory=datetime.now)
    is_active: bool = True
    web_client_count: int = 0
    metadata: dict[str, Any] = field(default_factory=dict)  # pyright: ignore[reportUnknownVariableType]

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization.

        Returns:
            Dictionary representation of the session
        """
        return {
            "session_id": self.session_id,
            "telegram_user_id": self.telegram_user_id,
            "telegram_username": self.telegram_username,
            "telegram_first_name": self.telegram_first_name,
            "telegram_last_name": self.telegram_last_name,
            "instance_id": self.instance_id,
            "message_history": [msg.to_dict() for msg in self.message_history],
            "created_at": self.created_at.isoformat(),
            "last_activity": self.last_activity.isoformat(),
            "is_active": self.is_active,
            "web_client_count": self.web_client_count,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "TelegramSession":
        """Create from dictionary.

        Args:
            data: Dictionary containing session data

        Returns:
            TelegramSession instance
        """
        return cls(
            session_id=data["session_id"],
            telegram_user_id=data["telegram_user_id"],
            telegram_username=data["telegram_username"],
            telegram_first_name=data["telegram_first_name"],
            telegram_last_name=data.get("telegram_last_name"),
            instance_id=data.get("instance_id"),
            message_history=[TelegramMessage.from_dict(msg) for msg in data.get("message_history", [])],
            created_at=datetime.fromisoformat(data["created_at"]),
            last_activity=datetime.fromisoformat(data["last_activity"]),
            is_active=data.get("is_active", True),
            web_client_count=data.get("web_client_count", 0),
            metadata=data.get("metadata", {}),
        )

    def add_message(self, message: TelegramMessage) -> None:
        """Add a message to the session history.

        Args:
            message: Message to add
        """
        self.message_history.append(message)
        self.last_activity = datetime.now()

    def get_display_name(self) -> str:
        """Get a display name for the session.

        Returns:
            Human-readable display name
        """
        if self.telegram_last_name:
            return f"{self.telegram_first_name} {self.telegram_last_name}"
        return self.telegram_first_name

    def get_last_message(self) -> TelegramMessage | None:
        """Get the last message in the session.

        Returns:
            Last message or None if no messages
        """
        return self.message_history[-1] if self.message_history else None


@dataclass
class WebSocketEvent:
    """A WebSocket event for client-server communication."""

    event_type: EventType
    data: dict[str, Any] = field(default_factory=dict)  # pyright: ignore[reportUnknownVariableType]

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization.

        Returns:
            Dictionary representation of the event
        """
        return {"type": self.event_type.value, **self.data}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "WebSocketEvent":
        """Create from dictionary.

        Args:
            data: Dictionary containing event data

        Returns:
            WebSocketEvent instance
        """
        event_type = EventType(data["type"])
        event_data = {k: v for k, v in data.items() if k != "type"}
        return cls(event_type=event_type, data=event_data)

    @classmethod
    def subscribe(cls, session_id: str, auth_token: str | None = None) -> "WebSocketEvent":
        """Create a subscribe event.

        Args:
            session_id: Session to subscribe to
            auth_token: Optional authentication token

        Returns:
            Subscribe event
        """
        data = {"session_id": session_id}
        if auth_token:
            data["auth_token"] = auth_token
        return cls(event_type=EventType.SUBSCRIBE, data=data)

    @classmethod
    def message(cls, message: TelegramMessage) -> "WebSocketEvent":
        """Create a message event.

        Args:
            message: Message to send

        Returns:
            Message event
        """
        return cls(event_type=EventType.MESSAGE, data={"message": message.to_dict()})

    @classmethod
    def typing(cls, is_typing: bool) -> "WebSocketEvent":
        """Create a typing indicator event.

        Args:
            is_typing: Whether the bot is typing

        Returns:
            Typing event
        """
        return cls(event_type=EventType.TYPING, data={"is_typing": is_typing})

    @classmethod
    def session_update(cls, session: TelegramSession) -> "WebSocketEvent":
        """Create a session update event.

        Args:
            session: Updated session

        Returns:
            Session update event
        """
        return cls(event_type=EventType.SESSION_UPDATE, data={"session": session.to_dict()})

    @classmethod
    def history(cls, messages: list[TelegramMessage]) -> "WebSocketEvent":
        """Create a history event.

        Args:
            messages: List of messages

        Returns:
            History event
        """
        return cls(event_type=EventType.HISTORY, data={"messages": [msg.to_dict() for msg in messages]})

    @classmethod
    def error(cls, error_message: str) -> "WebSocketEvent":
        """Create an error event.

        Args:
            error_message: Error message

        Returns:
            Error event
        """
        return cls(event_type=EventType.ERROR, data={"error": error_message})

    @classmethod
    def connected(cls, session_id: str) -> "WebSocketEvent":
        """Create a connected event.

        Args:
            session_id: Session ID

        Returns:
            Connected event
        """
        return cls(event_type=EventType.CONNECTED, data={"session_id": session_id})
