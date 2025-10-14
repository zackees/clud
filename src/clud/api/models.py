"""Data models for API message handling."""

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any


class ClientType(str, Enum):
    """Type of client making the request."""

    API = "api"
    TELEGRAM = "telegram"
    WEB = "web"
    WEBHOOK = "webhook"


class ExecutionStatus(str, Enum):
    """Status of command execution."""

    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"


@dataclass
class MessageRequest:
    """Request to handle a message from a client."""

    message: str
    session_id: str
    client_type: ClientType
    client_id: str
    working_directory: str | None = None
    metadata: dict[str, Any] = field(default_factory=lambda: {})

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "message": self.message,
            "session_id": self.session_id,
            "client_type": self.client_type.value,
            "client_id": self.client_id,
            "working_directory": self.working_directory,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "MessageRequest":
        """Create from dictionary."""
        return cls(
            message=data["message"],
            session_id=data["session_id"],
            client_type=ClientType(data["client_type"]),
            client_id=data["client_id"],
            working_directory=data.get("working_directory"),
            metadata=data.get("metadata", {}),
        )

    def validate(self) -> tuple[bool, str | None]:
        """Validate the request."""
        if not self.message or not self.message.strip():
            return False, "Message cannot be empty"
        if not self.session_id or not self.session_id.strip():
            return False, "Session ID cannot be empty"
        if not self.client_id or not self.client_id.strip():
            return False, "Client ID cannot be empty"
        return True, None


@dataclass
class MessageResponse:
    """Response from handling a message."""

    instance_id: str
    session_id: str
    status: ExecutionStatus
    message: str | None = None
    error: str | None = None
    metadata: dict[str, Any] = field(default_factory=lambda: {})

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "instance_id": self.instance_id,
            "session_id": self.session_id,
            "status": self.status.value,
            "message": self.message,
            "error": self.error,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "MessageResponse":
        """Create from dictionary."""
        return cls(
            instance_id=data["instance_id"],
            session_id=data["session_id"],
            status=ExecutionStatus(data["status"]),
            message=data.get("message"),
            error=data.get("error"),
            metadata=data.get("metadata", {}),
        )


@dataclass
class InstanceInfo:
    """Information about a clud instance."""

    instance_id: str
    session_id: str
    client_type: ClientType
    client_id: str
    status: ExecutionStatus
    created_at: datetime
    last_activity: datetime
    working_directory: str | None = None
    message_count: int = 0
    metadata: dict[str, Any] = field(default_factory=lambda: {})

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "instance_id": self.instance_id,
            "session_id": self.session_id,
            "client_type": self.client_type.value,
            "client_id": self.client_id,
            "status": self.status.value,
            "created_at": self.created_at.isoformat(),
            "last_activity": self.last_activity.isoformat(),
            "working_directory": self.working_directory,
            "message_count": self.message_count,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InstanceInfo":
        """Create from dictionary."""
        return cls(
            instance_id=data["instance_id"],
            session_id=data["session_id"],
            client_type=ClientType(data["client_type"]),
            client_id=data["client_id"],
            status=ExecutionStatus(data["status"]),
            created_at=datetime.fromisoformat(data["created_at"]),
            last_activity=datetime.fromisoformat(data["last_activity"]),
            working_directory=data.get("working_directory"),
            message_count=data.get("message_count", 0),
            metadata=data.get("metadata", {}),
        )


@dataclass
class ExecutionResult:
    """Result of executing a command in an instance."""

    instance_id: str
    status: ExecutionStatus
    output: str | None = None
    error: str | None = None
    exit_code: int | None = None
    metadata: dict[str, Any] = field(default_factory=lambda: {})

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "instance_id": self.instance_id,
            "status": self.status.value,
            "output": self.output,
            "error": self.error,
            "exit_code": self.exit_code,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ExecutionResult":
        """Create from dictionary."""
        return cls(
            instance_id=data["instance_id"],
            status=ExecutionStatus(data["status"]),
            output=data.get("output"),
            error=data.get("error"),
            exit_code=data.get("exit_code"),
            metadata=data.get("metadata", {}),
        )
