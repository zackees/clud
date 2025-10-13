"""
Data models for CLUD-CLUSTER.

These models define the core entities: Agent, Daemon, Session, TelegramBinding, and AuditEvent.
Based on the design specification in DESIGN.md (Data Models section).
"""

from datetime import datetime, timezone
from enum import Enum
from typing import Any, Literal
from uuid import UUID, uuid4

from pydantic import BaseModel, Field


class AgentStatus(str, Enum):
    """Agent status enumeration."""

    RUNNING = "running"
    IDLE = "idle"
    ERROR = "error"
    STOPPED = "stopped"


class Staleness(str, Enum):
    """Staleness indicator for agent freshness."""

    FRESH = "fresh"  # last_heartbeat < 15s
    STALE = "stale"  # 15s <= last_heartbeat < 90s
    DISCONNECTED = "disconnected"  # last_heartbeat >= 90s


class AgentMetrics(BaseModel):
    """Agent performance metrics."""

    cpu_percent: float = 0.0
    memory_mb: int = 0
    uptime_seconds: int = 0
    pty_bytes_sent: int = 0
    pty_bytes_received: int = 0


class Agent(BaseModel):
    """
    Agent represents a tracked clud process.

    State is owned by the daemon (source of truth) and eventually
    consistent in Cluster (view layer).
    """

    id: UUID = Field(default_factory=uuid4)
    daemon_id: UUID
    hostname: str
    pid: int
    cwd: str
    command: str
    status: AgentStatus = AgentStatus.RUNNING
    capabilities: list[str] = Field(default_factory=lambda: ["terminal"])

    # Timestamps
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    updated_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    last_heartbeat: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    stopped_at: datetime | None = None

    # Freshness tracking (computed field)
    staleness: Staleness = Staleness.FRESH

    # Daemon-reported state (ground truth)
    daemon_reported_status: str = "running"
    daemon_reported_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    # Metrics
    metrics: AgentMetrics = Field(default_factory=AgentMetrics)

    class Config:
        use_enum_values = True


class DaemonStatus(str, Enum):
    """Daemon connection status."""

    CONNECTED = "connected"
    DISCONNECTED = "disconnected"
    ERROR = "error"


class Daemon(BaseModel):
    """
    Daemon represents a local daemon process on a developer machine.

    Each daemon can track multiple agents (up to 50 recommended).
    """

    id: UUID = Field(default_factory=uuid4)
    hostname: str
    platform: str  # "linux", "darwin", "win32"
    version: str = "1.0.0-alpha"
    bind_address: str = "127.0.0.1:7565"
    status: DaemonStatus = DaemonStatus.CONNECTED
    agent_count: int = 0

    # Timestamps
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    last_seen: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    class Config:
        use_enum_values = True


class BindingMode(str, Enum):
    """Telegram binding mode."""

    ACTIVE = "active"  # Full control
    OBSERVER = "observer"  # Read-only


class TelegramBinding(BaseModel):
    """
    TelegramBinding links a Telegram chat to an agent.

    Supports one-controlling-chat-per-agent model.
    """

    id: UUID = Field(default_factory=uuid4)
    chat_id: int
    agent_id: UUID
    operator_id: str  # Telegram username or user_id
    mode: BindingMode = BindingMode.ACTIVE
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    class Config:
        use_enum_values = True


class SessionType(str, Enum):
    """Session access type."""

    WEB = "web"
    TELEGRAM = "telegram"
    API = "api"


class Session(BaseModel):
    """
    Session represents an authenticated operator session.

    Used for web UI access, API calls, and VS Code launches.
    """

    id: UUID = Field(default_factory=uuid4)
    operator_id: str
    type: SessionType
    token: str  # JWT token
    expires_at: datetime
    scopes: list[str] = Field(default_factory=list)  # ["agent:read", "agent:write", "vscode:launch"]

    class Config:
        use_enum_values = True


class EventResult(str, Enum):
    """Audit event result."""

    SUCCESS = "success"
    ERROR = "error"


class AuditEvent(BaseModel):
    """
    AuditEvent records operator actions for security and debugging.

    Append-only log of all control actions.
    """

    id: UUID = Field(default_factory=uuid4)
    operator_id: str
    event_type: str  # "agent_stop", "exec", "vscode_launch", etc.
    agent_id: UUID | None = None
    payload: dict[str, Any] = Field(default_factory=dict)
    result: EventResult
    timestamp: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    class Config:
        use_enum_values = True


# WebSocket Protocol Messages


class DaemonRegisterMessage(BaseModel):
    """Daemon registration message on control connection."""

    type: Literal["daemon_register"] = "daemon_register"
    daemon_id: UUID
    hostname: str
    platform: str
    version: str
    timestamp: int  # Unix timestamp
    agents: list[dict[str, Any]] = Field(default_factory=lambda: [])  # Full agent list for reconciliation


class RegisterAckMessage(BaseModel):
    """Cluster's acknowledgment of daemon registration."""

    type: Literal["register_ack"] = "register_ack"
    daemon_id: UUID
    session_token: str
    heartbeat_interval: int = 30  # seconds
    max_agents_per_pty_connection: int = 5
    reconciliation: dict[str, Any] = Field(default_factory=dict)


class AgentRegisterMessage(BaseModel):
    """Agent registration message."""

    type: Literal["agent_register"] = "agent_register"
    agent_id: UUID
    daemon_id: UUID
    pid: int
    cwd: str
    command: str
    env: dict[str, str] = Field(default_factory=dict)
    capabilities: list[str] = Field(default_factory=lambda: ["terminal"])
    pty_connection_id: str  # Which PTY connection pool handles this agent
    timestamp: int


class AgentRegisterAckMessage(BaseModel):
    """Acknowledgment of agent registration with PTY connection details."""

    type: Literal["agent_register_ack"] = "agent_register_ack"
    agent_id: UUID
    pty_ws_url: str  # Where to connect for PTY data


class HeartbeatMessage(BaseModel):
    """Periodic heartbeat from daemon to Cluster."""

    type: Literal["heartbeat"] = "heartbeat"
    daemon_id: UUID
    agents: list[dict[str, Any]] = Field(default_factory=lambda: [])  # Agent status updates
    pty_connections: list[dict[str, Any]] = Field(default_factory=lambda: [])
    timestamp: int


class AgentStopIntent(BaseModel):
    """Control intent: stop an agent."""

    type: Literal["agent_stop"] = "agent_stop"
    agent_id: UUID
    force: bool = False
    timeout_seconds: int = 10


class AgentExecIntent(BaseModel):
    """Control intent: execute command in agent's cwd."""

    type: Literal["agent_exec"] = "agent_exec"
    agent_id: UUID
    command: str
    cwd: str
    env: dict[str, str] = Field(default_factory=dict)
    timeout_seconds: int = 300


class VSCodeLaunchIntent(BaseModel):
    """Control intent: launch VS Code Server."""

    type: Literal["vscode_launch"] = "vscode_launch"
    agent_id: UUID
    port: int
    auth_token: str


class GetScrollbackIntent(BaseModel):
    """Request scrollback from daemon's ring buffer."""

    type: Literal["get_scrollback"] = "get_scrollback"
    agent_id: UUID
    lines: int = 1000


class ScrollbackDataMessage(BaseModel):
    """Response with scrollback data."""

    type: Literal["scrollback_data"] = "scrollback_data"
    agent_id: UUID
    lines: list[str]
    total_available: int


class AgentStoppedMessage(BaseModel):
    """Notification that agent has stopped."""

    type: Literal["agent_stopped"] = "agent_stopped"
    agent_id: UUID
    exit_code: int
    signal: int | None = None
    reason: str
    last_output: list[str] = Field(default_factory=list)
    timestamp: int
