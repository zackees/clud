"""Agent registry for tracking and managing agents."""

import json
import logging
import sqlite3
import time
from pathlib import Path
from typing import Any

from .models import AgentInfo, AgentStatus

logger = logging.getLogger(__name__)


class AgentRegistry:
    """Registry for tracking agents."""

    def __init__(self, db_path: Path | None = None, use_persistence: bool = False) -> None:
        """Initialize agent registry.

        Args:
            db_path: Path to SQLite database file (optional)
            use_persistence: Whether to use SQLite persistence (default: False, in-memory only)
        """
        self._agents: dict[str, AgentInfo] = {}
        self._use_persistence = use_persistence
        self._db_path = db_path
        self._conn: sqlite3.Connection | None = None

        if use_persistence and db_path:
            self._init_db()
            self._load_from_db()

    def _init_db(self) -> None:
        """Initialize SQLite database."""
        if not self._db_path:
            return

        self._conn = sqlite3.connect(str(self._db_path))
        cursor = self._conn.cursor()
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS agents (
                agent_id TEXT PRIMARY KEY,
                cwd TEXT NOT NULL,
                pid INTEGER NOT NULL,
                command TEXT NOT NULL,
                status TEXT NOT NULL,
                exit_code INTEGER,
                capabilities TEXT NOT NULL,
                created_at REAL NOT NULL,
                started_at REAL,
                last_heartbeat REAL NOT NULL,
                stopped_at REAL
            )
        """
        )
        self._conn.commit()

    def _load_from_db(self) -> None:
        """Load agents from database."""
        if not self._conn:
            return

        cursor = self._conn.cursor()
        cursor.execute("SELECT * FROM agents")
        for row in cursor.fetchall():
            agent_data = {
                "agent_id": row[0],
                "cwd": row[1],
                "pid": row[2],
                "command": row[3],
                "status": row[4],
                "exit_code": row[5],
                "capabilities": json.loads(row[6]),
                "created_at": row[7],
                "started_at": row[8],
                "last_heartbeat": row[9],
                "stopped_at": row[10],
            }
            self._agents[row[0]] = AgentInfo.from_dict(agent_data)

    def _save_to_db(self, agent: AgentInfo) -> None:
        """Save agent to database."""
        if not self._conn or not self._use_persistence:
            return

        cursor = self._conn.cursor()
        cursor.execute(
            """
            INSERT OR REPLACE INTO agents
            (agent_id, cwd, pid, command, status, exit_code, capabilities,
             created_at, started_at, last_heartbeat, stopped_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
            (
                agent.agent_id,
                agent.cwd,
                agent.pid,
                agent.command,
                agent.status.value,
                agent.exit_code,
                json.dumps(agent.capabilities),
                agent.created_at,
                agent.started_at,
                agent.last_heartbeat,
                agent.stopped_at,
            ),
        )
        self._conn.commit()

    def register(self, agent: AgentInfo) -> None:
        """Register a new agent.

        Args:
            agent: Agent information to register
        """
        self._agents[agent.agent_id] = agent
        self._save_to_db(agent)
        logger.info(f"Registered agent {agent.agent_id} (pid={agent.pid}, cwd={agent.cwd})")

    def update_heartbeat(self, agent_id: str, status: AgentStatus | None = None, **kwargs: Any) -> bool:
        """Update agent heartbeat and optionally status.

        Args:
            agent_id: Agent identifier
            status: Optional new status
            **kwargs: Additional fields to update

        Returns:
            True if agent was found and updated, False otherwise
        """
        agent = self._agents.get(agent_id)
        if not agent:
            logger.warning(f"Heartbeat for unknown agent {agent_id}")
            return False

        agent.last_heartbeat = time.time()
        if status:
            agent.status = status

        # Update any additional fields
        for key, value in kwargs.items():
            if hasattr(agent, key):
                setattr(agent, key, value)

        self._save_to_db(agent)
        logger.debug(f"Heartbeat for agent {agent_id} (status={agent.status.value})")
        return True

    def get(self, agent_id: str) -> AgentInfo | None:
        """Get agent by ID.

        Args:
            agent_id: Agent identifier

        Returns:
            Agent info if found, None otherwise
        """
        return self._agents.get(agent_id)

    def list_all(self) -> list[AgentInfo]:
        """List all agents.

        Returns:
            List of all agent info
        """
        return list(self._agents.values())

    def list_by_status(self, status: AgentStatus) -> list[AgentInfo]:
        """List agents by status.

        Args:
            status: Status to filter by

        Returns:
            List of agents with the given status
        """
        return [agent for agent in self._agents.values() if agent.status == status]

    def list_stale(self, threshold_seconds: float = 15.0) -> list[AgentInfo]:
        """List agents with stale heartbeats.

        Args:
            threshold_seconds: Number of seconds after which agent is considered stale

        Returns:
            List of agents with stale heartbeats
        """
        now = time.time()
        return [agent for agent in self._agents.values() if (now - agent.last_heartbeat) > threshold_seconds and agent.status in (AgentStatus.STARTING, AgentStatus.RUNNING)]

    def mark_stopped(self, agent_id: str, exit_code: int = 0) -> bool:
        """Mark agent as stopped.

        Args:
            agent_id: Agent identifier
            exit_code: Exit code of the agent process

        Returns:
            True if agent was found and marked stopped, False otherwise
        """
        agent = self._agents.get(agent_id)
        if not agent:
            return False

        agent.status = AgentStatus.STOPPED if exit_code == 0 else AgentStatus.FAILED
        agent.exit_code = exit_code
        agent.stopped_at = time.time()
        self._save_to_db(agent)
        logger.info(f"Agent {agent_id} stopped with exit code {exit_code}")
        return True

    def close(self) -> None:
        """Close the registry and database connection."""
        if self._conn:
            self._conn.close()
            self._conn = None
