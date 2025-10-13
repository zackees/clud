"""Data models for daemon and agent tracking."""

import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


def _empty_str_dict() -> dict[str, str]:
    """Factory for empty string dict."""
    return {}


class AgentStatus(Enum):
    """Agent lifecycle status."""

    STARTING = "starting"
    RUNNING = "running"
    STOPPED = "stopped"
    FAILED = "failed"


@dataclass
class AgentInfo:
    """Information about a tracked agent."""

    agent_id: str
    """Unique agent identifier"""

    cwd: str
    """Working directory where agent is running"""

    pid: int
    """Process ID of the agent"""

    command: str
    """Command being executed by the agent"""

    status: AgentStatus = AgentStatus.STARTING
    """Current status of the agent"""

    exit_code: int | None = None
    """Exit code if agent has stopped"""

    capabilities: dict[str, str] = field(default_factory=_empty_str_dict)
    """Agent capabilities and features"""

    created_at: float = field(default_factory=time.time)
    """Timestamp when agent was created"""

    started_at: float | None = None
    """Timestamp when agent started running"""

    last_heartbeat: float = field(default_factory=time.time)
    """Timestamp of last heartbeat"""

    stopped_at: float | None = None
    """Timestamp when agent stopped"""

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "agent_id": self.agent_id,
            "cwd": self.cwd,
            "pid": self.pid,
            "command": self.command,
            "status": self.status.value,
            "exit_code": self.exit_code,
            "capabilities": self.capabilities,
            "created_at": self.created_at,
            "started_at": self.started_at,
            "last_heartbeat": self.last_heartbeat,
            "stopped_at": self.stopped_at,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "AgentInfo":
        """Create from dictionary."""
        # Convert status string to enum
        status = AgentStatus(data["status"]) if isinstance(data["status"], str) else data["status"]
        return cls(
            agent_id=data["agent_id"],
            cwd=data["cwd"],
            pid=data["pid"],
            command=data["command"],
            status=status,
            exit_code=data.get("exit_code"),
            capabilities=data.get("capabilities", {}),
            created_at=data.get("created_at", time.time()),
            started_at=data.get("started_at"),
            last_heartbeat=data.get("last_heartbeat", time.time()),
            stopped_at=data.get("stopped_at"),
        )
