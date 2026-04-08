"""Data models for API message handling."""

from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any, cast


def _metadata_dict() -> dict[str, Any]:
    """Return a typed empty metadata mapping."""
    return {}


def _string_list() -> list[str]:
    """Return a typed empty list of string arguments."""
    return []


def _coerce_agent_args(value: object) -> list[str]:
    """Normalize stored agent arg payloads to a list of strings."""
    if value is None:
        return []
    if isinstance(value, list):
        items = cast(list[object], value)
        return [str(item) for item in items]
    if isinstance(value, tuple):
        items = cast(tuple[object, ...], value)
        return [str(item) for item in items]
    return [str(value)]


def _coerce_metadata(value: object) -> dict[str, Any]:
    """Normalize metadata payloads to a plain string-keyed dict."""
    if not isinstance(value, Mapping):
        return {}
    mapping = cast(Mapping[object, object], value)
    return {str(key): raw_value for key, raw_value in mapping.items()}


def _get_object(mapping: Mapping[str, object], key: str) -> object | None:
    """Fetch a raw object from a mapping."""
    value = mapping.get(key)
    return value if value is not None else None


def _get_string(mapping: Mapping[str, object], key: str) -> str | None:
    """Fetch a string value from a mapping when present."""
    value = mapping.get(key)
    return value if isinstance(value, str) else None


def _get_bool(mapping: Mapping[str, object], key: str) -> bool | None:
    """Fetch a bool value from a mapping when present."""
    value = mapping.get(key)
    return value if isinstance(value, bool) else None


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


class InvocationMode(str, Enum):
    """How the frontend wants clud invoked."""

    MESSAGE = "message"
    PROMPT = "prompt"


@dataclass
class MessageRequest:
    """Request to handle a message from a client."""

    message: str
    session_id: str
    client_type: ClientType
    client_id: str
    working_directory: str | None = None
    invocation_mode: InvocationMode = InvocationMode.MESSAGE
    session_model: str | None = None
    agent_args: list[str] = field(default_factory=_string_list)
    metadata: dict[str, Any] = field(default_factory=_metadata_dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "message": self.message,
            "session_id": self.session_id,
            "client_type": self.client_type.value,
            "client_id": self.client_id,
            "working_directory": self.working_directory,
            "invocation_mode": self.invocation_mode.value,
            "session_model": self.session_model,
            "agent_args": self.agent_args,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "MessageRequest":
        """Create from dictionary."""
        payload: Mapping[str, object] = data
        metadata = _coerce_metadata(_get_object(payload, "metadata"))

        invocation_mode = _get_string(payload, "invocation_mode") or _get_string(metadata, "invocation_mode")
        if invocation_mode is None:
            use_print_flag = _get_bool(payload, "use_print_flag")
            if use_print_flag is None:
                use_print_flag = _get_bool(metadata, "use_print_flag")
            invocation_mode = "prompt" if use_print_flag else "message"

        session_model = _get_string(payload, "session_model")
        if session_model is None:
            session_model = _get_string(payload, "backend")
        if session_model is None:
            session_model = _get_string(metadata, "session_model") or _get_string(metadata, "backend")

        agent_args = _get_object(payload, "agent_args")
        if agent_args is None:
            agent_args = _get_object(payload, "claude_args")
        if agent_args is None:
            agent_args = _get_object(metadata, "agent_args")
        if agent_args is None:
            agent_args = _get_object(metadata, "claude_args")

        return cls(
            message=str(data["message"]),
            session_id=str(data["session_id"]),
            client_type=ClientType(str(data["client_type"])),
            client_id=str(data["client_id"]),
            working_directory=_get_string(payload, "working_directory"),
            invocation_mode=InvocationMode(invocation_mode),
            session_model=session_model,
            agent_args=_coerce_agent_args(agent_args),
            metadata=metadata,
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
    metadata: dict[str, Any] = field(default_factory=_metadata_dict)

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
            metadata=data.get("metadata", {}) if isinstance(data.get("metadata", {}), dict) else {},
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
    metadata: dict[str, Any] = field(default_factory=_metadata_dict)

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
            metadata=data.get("metadata", {}) if isinstance(data.get("metadata", {}), dict) else {},
        )


@dataclass
class ExecutionResult:
    """Result of executing a command in an instance."""

    instance_id: str
    status: ExecutionStatus
    output: str | None = None
    error: str | None = None
    exit_code: int | None = None
    metadata: dict[str, Any] = field(default_factory=_metadata_dict)

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
            metadata=data.get("metadata", {}) if isinstance(data.get("metadata", {}), dict) else {},
        )
