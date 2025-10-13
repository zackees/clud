"""
WebSocket handlers for CLUD-CLUSTER.

Handles three types of WebSocket connections:
1. Daemon control connections - daemon registration, heartbeats, control intents
2. PTY pool connections - binary PTY data from daemons (pooled, 5-10 agents per connection)
3. Browser terminal connections - PTY data to browser xterm.js instances

Based on DESIGN.md WebSocket Protocol section.
"""

import json
import logging
from datetime import datetime, timezone
from typing import Any
from uuid import UUID

from fastapi import WebSocket, WebSocketDisconnect
from sqlalchemy.ext.asyncio import AsyncSession

from .database import (
    AgentDB,
    DaemonDB,
    get_agent_by_id,
    get_daemon_by_id,
    update_agent_staleness,
)
from .models import (
    AgentRegisterAckMessage,
    AgentRegisterMessage,
    AgentStatus,
    AgentStoppedMessage,
    DaemonRegisterMessage,
    DaemonStatus,
    HeartbeatMessage,
    RegisterAckMessage,
    Staleness,
)

logger = logging.getLogger(__name__)


# Connection tracking
# Maps daemon_id -> WebSocket for control connections
daemon_control_connections: dict[UUID, WebSocket] = {}

# Maps pool_id -> WebSocket for PTY connections
pty_pool_connections: dict[str, WebSocket] = {}

# Maps agent_id -> WebSocket for browser terminal connections
terminal_connections: dict[UUID, WebSocket] = {}

# Maps agent_id -> pool_id for routing PTY data
agent_to_pool_mapping: dict[UUID, str] = {}

# Set of event subscriber WebSockets
event_subscribers: set[WebSocket] = set()


class WebSocketConnectionManager:
    """Manages WebSocket connections and message routing."""

    def __init__(self, db_session_factory: Any) -> None:
        """
        Initialize connection manager.

        Args:
            db_session_factory: Factory function to create database sessions
        """
        self.db_session_factory = db_session_factory

    async def handle_daemon_control(self, websocket: WebSocket, daemon_id: str) -> None:
        """
        Handle daemon control WebSocket connection.

        This connection receives:
        - daemon_register (on connect)
        - heartbeat (periodic)
        - agent_register (when new agent spawned)
        - agent_stopped (when agent exits)

        This connection sends:
        - register_ack (response to daemon_register)
        - agent_register_ack (response to agent_register)
        - Control intents (agent_stop, agent_exec, vscode_launch, get_scrollback)
        """
        await websocket.accept()
        daemon_uuid = UUID(daemon_id)
        daemon_control_connections[daemon_uuid] = websocket

        logger.info(f"Daemon control connection established: {daemon_id}")

        try:
            while True:
                # Receive message from daemon
                raw_message = await websocket.receive_text()
                message_data = json.loads(raw_message)
                msg_type = message_data.get("type")

                logger.debug(f"Received {msg_type} from daemon {daemon_id}")

                # Route message to appropriate handler
                if msg_type == "daemon_register":
                    await self._handle_daemon_register(websocket, daemon_uuid, message_data)
                elif msg_type == "heartbeat":
                    await self._handle_heartbeat(daemon_uuid, message_data)
                elif msg_type == "agent_register":
                    await self._handle_agent_register(websocket, message_data)
                elif msg_type == "agent_stopped":
                    await self._handle_agent_stopped(message_data)
                else:
                    logger.warning(f"Unknown message type from daemon {daemon_id}: {msg_type}")

        except WebSocketDisconnect:
            logger.info(f"Daemon {daemon_id} disconnected")
        except Exception as e:
            logger.exception(f"Error in daemon control connection {daemon_id}: {e}")
        finally:
            # Clean up connection
            if daemon_uuid in daemon_control_connections:
                del daemon_control_connections[daemon_uuid]

            # Update daemon status in database
            session: AsyncSession
            async for session in self.db_session_factory():
                daemon_db = await get_daemon_by_id(session, daemon_uuid)
                if daemon_db:
                    daemon_db.status = DaemonStatus.DISCONNECTED.value
                    daemon_db.last_seen = datetime.now(timezone.utc)
                    await session.flush()

                    # Broadcast daemon_disconnected event
                    await self.broadcast_event(
                        {
                            "type": "daemon_disconnected",
                            "daemon": {
                                "id": str(daemon_uuid),
                                "hostname": daemon_db.hostname,
                                "status": "disconnected",
                            },
                        }
                    )
                break

            logger.info(f"Daemon control connection cleaned up: {daemon_id}")

    async def _handle_daemon_register(self, websocket: WebSocket, daemon_uuid: UUID, message_data: dict[str, Any]) -> None:
        """
        Handle daemon_register message.

        Creates or updates daemon in database and sends register_ack.
        """
        try:
            msg = DaemonRegisterMessage(**message_data)
        except Exception as e:
            logger.error(f"Invalid daemon_register message: {e}")
            await websocket.send_json({"type": "error", "message": "Invalid registration"})
            return

        logger.info(f"Daemon registration: {msg.hostname} (platform={msg.platform}, version={msg.version}, agents={len(msg.agents)})")

        # Store daemon in database
        session: AsyncSession
        async for session in self.db_session_factory():
            daemon_db = await get_daemon_by_id(session, daemon_uuid)

            if daemon_db:
                # Update existing daemon
                daemon_db.hostname = msg.hostname
                daemon_db.platform = msg.platform
                daemon_db.version = msg.version
                daemon_db.status = DaemonStatus.CONNECTED.value
                daemon_db.agent_count = len(msg.agents)
                daemon_db.last_seen = datetime.now(timezone.utc)
            else:
                # Create new daemon
                daemon_db = DaemonDB(
                    id=daemon_uuid,
                    hostname=msg.hostname,
                    platform=msg.platform,
                    version=msg.version,
                    bind_address="127.0.0.1:7565",
                    status=DaemonStatus.CONNECTED.value,
                    agent_count=len(msg.agents),
                    created_at=datetime.now(timezone.utc),
                    last_seen=datetime.now(timezone.utc),
                )
                session.add(daemon_db)

            await session.flush()

            # Reconcile agents (update database with daemon's agent list)
            reconciliation = await self._reconcile_agents(session, daemon_uuid, msg.agents)

            # Send acknowledgment
            ack = RegisterAckMessage(
                daemon_id=daemon_uuid,
                session_token="mock_token",  # TODO: Generate real JWT token
                heartbeat_interval=30,
                max_agents_per_pty_connection=5,
                reconciliation=reconciliation,
            )

            await websocket.send_json(ack.model_dump(mode="json"))
            logger.info(f"Sent register_ack to daemon {daemon_uuid}")

            # Broadcast daemon_connected event
            await self.broadcast_event(
                {
                    "type": "daemon_connected",
                    "daemon": {
                        "id": str(daemon_uuid),
                        "hostname": msg.hostname,
                        "platform": msg.platform,
                        "status": "connected",
                        "agent_count": len(msg.agents),
                    },
                }
            )

            break

    async def _reconcile_agents(self, session: AsyncSession, daemon_uuid: UUID, agents_data: list[dict[str, Any]]) -> dict[str, list[str]]:
        """
        Reconcile daemon's agent list with Cluster's database.

        Returns reconciliation info for daemon to sync state.
        """
        daemon_agent_ids = {UUID(a["id"]) for a in agents_data if "id" in a}

        # Find agents in database for this daemon
        from sqlalchemy import select

        result = await session.execute(select(AgentDB).where(AgentDB.daemon_id == daemon_uuid))
        db_agents = list(result.scalars().all())
        db_agent_ids = {a.id for a in db_agents}

        # Agents in daemon but not in DB -> register them
        new_agents = daemon_agent_ids - db_agent_ids

        # Agents in DB but not in daemon -> mark as stopped
        stopped_agents = db_agent_ids - daemon_agent_ids

        for agent_id in stopped_agents:
            agent = await get_agent_by_id(session, agent_id)
            if agent and agent.status != AgentStatus.STOPPED.value:
                agent.status = AgentStatus.STOPPED.value
                agent.daemon_reported_status = "stopped"
                agent.stopped_at = datetime.now(timezone.utc)
                agent.updated_at = datetime.now(timezone.utc)
                await session.flush()

        logger.info(f"Reconciliation for daemon {daemon_uuid}: {len(new_agents)} new, {len(stopped_agents)} stopped, {len(daemon_agent_ids & db_agent_ids)} existing")

        return {
            "new_agents": [str(a) for a in new_agents],
            "stopped_agents": [str(a) for a in stopped_agents],
            "existing_agents": [str(a) for a in (daemon_agent_ids & db_agent_ids)],
        }

    async def _handle_heartbeat(self, daemon_uuid: UUID, message_data: dict[str, Any]) -> None:
        """
        Handle heartbeat message from daemon.

        Updates daemon last_seen and agent status/metrics.
        """
        try:
            msg = HeartbeatMessage(**message_data)
        except Exception as e:
            logger.error(f"Invalid heartbeat message: {e}")
            return

        logger.debug(f"Heartbeat from daemon {daemon_uuid}: {len(msg.agents)} agents")

        session: AsyncSession
        async for session in self.db_session_factory():
            # Update daemon last_seen
            daemon_db = await get_daemon_by_id(session, daemon_uuid)
            if daemon_db:
                daemon_db.last_seen = datetime.now(timezone.utc)
                daemon_db.agent_count = len(msg.agents)
                await session.flush()

            # Update agent statuses
            for agent_data in msg.agents:
                agent_id = UUID(agent_data.get("id"))
                agent_db = await get_agent_by_id(session, agent_id)

                if agent_db:
                    # Update status from daemon
                    agent_db.last_heartbeat = datetime.now(timezone.utc)
                    agent_db.daemon_reported_status = agent_data.get("status", "running")
                    agent_db.daemon_reported_at = datetime.now(timezone.utc)
                    agent_db.status = agent_data.get("status", "running")

                    # Update metrics if provided
                    if "metrics" in agent_data:
                        agent_db.metrics = agent_data["metrics"]

                    # Compute staleness
                    await update_agent_staleness(session, agent_db)

                    await session.flush()

                    # Broadcast agent_updated event
                    await self.broadcast_event(
                        {
                            "type": "agent_updated",
                            "agent": {
                                "id": str(agent_id),
                                "daemon_id": str(agent_db.daemon_id),
                                "status": agent_db.status,
                                "staleness": agent_db.staleness,
                                "metrics": agent_db.metrics,
                            },
                        }
                    )

            break

    async def _handle_agent_register(self, websocket: WebSocket, message_data: dict[str, Any]) -> None:
        """
        Handle agent_register message.

        Creates agent in database and sends agent_register_ack with PTY connection details.
        """
        try:
            msg = AgentRegisterMessage(**message_data)
        except Exception as e:
            logger.error(f"Invalid agent_register message: {e}")
            await websocket.send_json({"type": "error", "message": "Invalid agent registration"})
            return

        logger.info(f"Agent registration: {msg.agent_id} (pid={msg.pid}, cwd={msg.cwd}, pty_pool={msg.pty_connection_id})")

        session: AsyncSession
        async for session in self.db_session_factory():
            # Get daemon hostname
            daemon_db = await get_daemon_by_id(session, msg.daemon_id)
            hostname = daemon_db.hostname if daemon_db else "unknown"

            # Check if agent already exists
            agent_db = await get_agent_by_id(session, msg.agent_id)

            if agent_db:
                # Update existing agent
                agent_db.pid = msg.pid
                agent_db.cwd = msg.cwd
                agent_db.command = msg.command
                agent_db.status = AgentStatus.RUNNING.value
                agent_db.capabilities = msg.capabilities
                agent_db.daemon_reported_status = "running"
                agent_db.daemon_reported_at = datetime.now(timezone.utc)
                agent_db.last_heartbeat = datetime.now(timezone.utc)
                agent_db.updated_at = datetime.now(timezone.utc)
                agent_db.staleness = Staleness.FRESH.value
            else:
                # Create new agent
                agent_db = AgentDB(
                    id=msg.agent_id,
                    daemon_id=msg.daemon_id,
                    hostname=hostname,
                    pid=msg.pid,
                    cwd=msg.cwd,
                    command=msg.command,
                    status=AgentStatus.RUNNING.value,
                    capabilities=msg.capabilities,
                    created_at=datetime.now(timezone.utc),
                    updated_at=datetime.now(timezone.utc),
                    last_heartbeat=datetime.now(timezone.utc),
                    staleness=Staleness.FRESH.value,
                    daemon_reported_status="running",
                    daemon_reported_at=datetime.now(timezone.utc),
                    metrics={},
                )
                session.add(agent_db)

            await session.flush()

            # Map agent to PTY pool
            agent_to_pool_mapping[msg.agent_id] = msg.pty_connection_id

            # Send acknowledgment with PTY WebSocket URL
            ack = AgentRegisterAckMessage(
                agent_id=msg.agent_id,
                pty_ws_url=f"ws://localhost:8000/ws/pty/pool-{msg.pty_connection_id}",
            )

            await websocket.send_json(ack.model_dump(mode="json"))
            logger.info(f"Sent agent_register_ack for agent {msg.agent_id}")

            # Broadcast agent_register event
            await self.broadcast_event(
                {
                    "type": "agent_register",
                    "agent": {
                        "id": str(msg.agent_id),
                        "daemon_id": str(msg.daemon_id),
                        "hostname": hostname,
                        "pid": msg.pid,
                        "cwd": msg.cwd,
                        "command": msg.command,
                        "status": "running",
                        "capabilities": msg.capabilities,
                        "staleness": "fresh",
                    },
                }
            )

            break

    async def _handle_agent_stopped(self, message_data: dict[str, Any]) -> None:
        """
        Handle agent_stopped notification.

        Marks agent as stopped in database.
        """
        try:
            msg = AgentStoppedMessage(**message_data)
        except Exception as e:
            logger.error(f"Invalid agent_stopped message: {e}")
            return

        logger.info(f"Agent stopped: {msg.agent_id} (exit_code={msg.exit_code}, reason={msg.reason})")

        session: AsyncSession
        async for session in self.db_session_factory():
            agent_db = await get_agent_by_id(session, msg.agent_id)

            if agent_db:
                agent_db.status = AgentStatus.STOPPED.value
                agent_db.daemon_reported_status = "stopped"
                agent_db.stopped_at = datetime.now(timezone.utc)
                agent_db.updated_at = datetime.now(timezone.utc)
                agent_db.staleness = Staleness.DISCONNECTED.value
                await session.flush()

            # Clean up agent-to-pool mapping
            if msg.agent_id in agent_to_pool_mapping:
                del agent_to_pool_mapping[msg.agent_id]

            # Broadcast agent_stopped event
            if agent_db:
                await self.broadcast_event(
                    {
                        "type": "agent_stopped",
                        "agent": {
                            "id": str(msg.agent_id),
                            "daemon_id": str(agent_db.daemon_id),
                            "status": "stopped",
                            "exit_code": msg.exit_code,
                        },
                    }
                )

            break

    async def handle_pty_pool(self, websocket: WebSocket, pool_id: str) -> None:
        """
        Handle PTY pool WebSocket connection.

        This connection carries binary PTY data for multiple agents (5-10 per pool).
        Each binary frame has a 16-byte header with agent_id.

        Frame format:
        - Bytes 0-15: agent_id (UUID as 16 bytes)
        - Bytes 16+: PTY data

        Routes data to appropriate browser terminal connections.
        """
        await websocket.accept()
        pty_pool_connections[pool_id] = websocket

        logger.info(f"PTY pool connection established: {pool_id}")

        try:
            while True:
                # Receive binary frame
                data = await websocket.receive_bytes()

                if len(data) < 16:
                    logger.warning(f"Invalid PTY frame from pool {pool_id}: too short")
                    continue

                # Extract agent_id from first 16 bytes
                agent_id_bytes = data[:16]
                pty_data = data[16:]

                try:
                    # Convert bytes to UUID
                    agent_id = UUID(bytes=agent_id_bytes)
                except Exception as e:
                    logger.error(f"Invalid agent_id in PTY frame: {e}")
                    continue

                # Route to browser terminal if connected
                if agent_id in terminal_connections:
                    terminal_ws = terminal_connections[agent_id]
                    try:
                        await terminal_ws.send_bytes(pty_data)
                        logger.debug(f"Routed {len(pty_data)} bytes from pool {pool_id} to agent {agent_id}")
                    except Exception as e:
                        logger.error(f"Error sending to terminal {agent_id}: {e}")
                        # Terminal connection is dead, clean it up
                        del terminal_connections[agent_id]
                else:
                    # No browser connected, drop the data (daemon buffers in ring buffer)
                    logger.debug(f"No terminal connected for agent {agent_id}, dropping {len(pty_data)} bytes")

        except WebSocketDisconnect:
            logger.info(f"PTY pool {pool_id} disconnected")
        except Exception as e:
            logger.exception(f"Error in PTY pool connection {pool_id}: {e}")
        finally:
            # Clean up connection
            if pool_id in pty_pool_connections:
                del pty_pool_connections[pool_id]

            logger.info(f"PTY pool connection cleaned up: {pool_id}")

    async def handle_browser_terminal(self, websocket: WebSocket, agent_id: str) -> None:
        """
        Handle browser terminal WebSocket connection.

        This connection receives PTY data from Cluster and sends user input to daemon.

        Browser -> Cluster -> Daemon (control connection)
        Daemon -> Cluster (PTY pool) -> Browser
        """
        await websocket.accept()
        agent_uuid = UUID(agent_id)
        terminal_connections[agent_uuid] = websocket

        logger.info(f"Browser terminal connection established for agent {agent_id}")

        try:
            # Send scrollback history if available
            # TODO: Request scrollback from daemon via control connection

            while True:
                # Receive user input from browser
                data = await websocket.receive_bytes()

                # Forward input to daemon via control connection
                # Find which daemon owns this agent
                session: AsyncSession
                async for session in self.db_session_factory():
                    agent_db = await get_agent_by_id(session, agent_uuid)
                    if agent_db:
                        daemon_id = agent_db.daemon_id

                        if daemon_id in daemon_control_connections:
                            daemon_ws = daemon_control_connections[daemon_id]

                            # Send terminal_input control message
                            input_msg = {
                                "type": "terminal_input",
                                "agent_id": str(agent_uuid),
                                "data": data.hex(),  # Send as hex string in JSON
                            }

                            try:
                                await daemon_ws.send_json(input_msg)
                                logger.debug(f"Sent {len(data)} bytes of input for agent {agent_id} to daemon {daemon_id}")
                            except Exception as e:
                                logger.error(f"Error sending input to daemon {daemon_id}: {e}")
                        else:
                            logger.warning(f"Daemon {daemon_id} not connected, cannot send input for agent {agent_id}")
                    break

        except WebSocketDisconnect:
            logger.info(f"Browser terminal {agent_id} disconnected")
        except Exception as e:
            logger.exception(f"Error in browser terminal connection {agent_id}: {e}")
        finally:
            # Clean up connection
            if agent_uuid in terminal_connections:
                del terminal_connections[agent_uuid]

            logger.info(f"Browser terminal connection cleaned up: {agent_id}")

    async def send_control_intent(self, daemon_id: UUID, intent: dict[str, Any]) -> None:
        """
        Send a control intent to a daemon.

        Used for:
        - agent_stop
        - agent_exec
        - vscode_launch
        - get_scrollback
        """
        if daemon_id not in daemon_control_connections:
            raise ValueError(f"Daemon {daemon_id} is not connected")

        daemon_ws = daemon_control_connections[daemon_id]
        await daemon_ws.send_json(intent)
        logger.info(f"Sent control intent {intent.get('type')} to daemon {daemon_id}")

    async def broadcast_event(self, event: dict[str, Any]) -> None:
        """
        Broadcast an event to all event subscribers.

        Used for:
        - agent_updated
        - agent_register
        - agent_stopped
        - daemon_connected
        - daemon_disconnected
        """
        if not event_subscribers:
            return

        # Create list to track dead connections
        dead_connections: list[WebSocket] = []

        for ws in event_subscribers:
            try:
                await ws.send_json(event)
            except Exception as e:
                logger.error(f"Error broadcasting event to subscriber: {e}")
                dead_connections.append(ws)

        # Clean up dead connections
        for ws in dead_connections:
            event_subscribers.discard(ws)

        logger.debug(f"Broadcast event {event.get('type')} to {len(event_subscribers)} subscribers")

    async def handle_events(self, websocket: WebSocket) -> None:
        """
        Handle event subscription WebSocket connection.

        Clients connect to receive real-time updates about agents and daemons:
        - agent_updated (status, metrics changes)
        - agent_register (new agent)
        - agent_stopped (agent exited)
        - daemon_connected (daemon online)
        - daemon_disconnected (daemon offline)
        """
        await websocket.accept()
        event_subscribers.add(websocket)

        logger.info(f"Event subscriber connected (total: {len(event_subscribers)})")

        try:
            # Keep connection alive and wait for client disconnect
            while True:
                # Receive ping/pong or other messages from client
                try:
                    await websocket.receive_text()
                except Exception:
                    break

        except WebSocketDisconnect:
            logger.info("Event subscriber disconnected")
        except Exception as e:
            logger.exception(f"Error in event subscription: {e}")
        finally:
            # Clean up connection
            event_subscribers.discard(websocket)
            logger.info(f"Event subscriber cleaned up (remaining: {len(event_subscribers)})")
