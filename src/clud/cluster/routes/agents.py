"""
Agent management API routes.

Provides REST endpoints for:
- Listing agents
- Getting agent details
- Agent control operations (stop, exec, scrollback)
"""

from typing import TYPE_CHECKING
from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException

from ..auth import TokenData
from ..auth_dependencies import require_auth
from ..database import Database, get_agent_by_id
from ..database import list_agents as db_list_agents
from ..models import Agent, AgentExecIntent, AgentMetrics, AgentStatus, AgentStopIntent, GetScrollbackIntent, Staleness

if TYPE_CHECKING:
    from ..websocket_handlers import WebSocketConnectionManager

# This will be set by app.py during initialization
db: Database | None = None
ws_manager: "WebSocketConnectionManager | None" = None


def init_agents_routes(database: Database, websocket_manager: "WebSocketConnectionManager") -> None:
    """Initialize module-level dependencies."""
    global db, ws_manager
    db = database
    ws_manager = websocket_manager


router = APIRouter(prefix="/api/v1/agents", tags=["agents"])


@router.get("", response_model=list[Agent])
async def list_agents(daemon_id: str | None = None) -> list[Agent]:
    """
    List all agents.

    Query parameters:
    - daemon_id: Optional filter by daemon ID
    """
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    async with db.get_session() as session:
        daemon_uuid = UUID(daemon_id) if daemon_id else None
        agents_db = await db_list_agents(session, daemon_uuid)

        # Convert DB models to Pydantic models
        return [
            Agent(
                id=agent.id,
                daemon_id=agent.daemon_id,
                hostname=agent.hostname,
                pid=agent.pid,
                cwd=agent.cwd,
                command=agent.command,
                status=AgentStatus(agent.status),
                capabilities=agent.capabilities,
                created_at=agent.created_at,
                updated_at=agent.updated_at,
                last_heartbeat=agent.last_heartbeat,
                stopped_at=agent.stopped_at,
                staleness=Staleness(agent.staleness),
                daemon_reported_status=agent.daemon_reported_status,
                daemon_reported_at=agent.daemon_reported_at,
                metrics=AgentMetrics(**agent.metrics) if isinstance(agent.metrics, dict) else agent.metrics,
            )
            for agent in agents_db
        ]


@router.get("/{agent_id}", response_model=Agent)
async def get_agent(agent_id: str) -> Agent:
    """Get agent details by ID."""
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    async with db.get_session() as session:
        agent_db = await get_agent_by_id(session, UUID(agent_id))
        if not agent_db:
            raise HTTPException(status_code=404, detail="Agent not found")

        return Agent(
            id=agent_db.id,
            daemon_id=agent_db.daemon_id,
            hostname=agent_db.hostname,
            pid=agent_db.pid,
            cwd=agent_db.cwd,
            command=agent_db.command,
            status=AgentStatus(agent_db.status),
            capabilities=agent_db.capabilities,
            created_at=agent_db.created_at,
            updated_at=agent_db.updated_at,
            last_heartbeat=agent_db.last_heartbeat,
            stopped_at=agent_db.stopped_at,
            staleness=Staleness(agent_db.staleness),
            daemon_reported_status=agent_db.daemon_reported_status,
            daemon_reported_at=agent_db.daemon_reported_at,
            metrics=AgentMetrics(**agent_db.metrics) if isinstance(agent_db.metrics, dict) else agent_db.metrics,
        )


@router.post("/{agent_id}/stop")
async def stop_agent(
    agent_id: str,
    force: bool = False,
    timeout_seconds: int = 10,
    token: TokenData = Depends(require_auth),
) -> dict[str, str]:
    """
    Stop an agent.

    Sends agent_stop intent to the daemon that owns the agent.
    Requires authentication with agent:write scope.
    """
    if not db or not ws_manager:
        raise HTTPException(status_code=503, detail="Service not available")

    async with db.get_session() as session:
        agent_db = await get_agent_by_id(session, UUID(agent_id))
        if not agent_db:
            raise HTTPException(status_code=404, detail="Agent not found")

        daemon_id = agent_db.daemon_id

        try:
            intent = AgentStopIntent(
                agent_id=UUID(agent_id),
                force=force,
                timeout_seconds=timeout_seconds,
            )
            await ws_manager.send_control_intent(daemon_id, intent.model_dump(mode="json"))

            return {"status": "sent", "message": f"Stop intent sent to daemon {daemon_id}"}
        except ValueError as e:
            raise HTTPException(status_code=503, detail=str(e)) from e


@router.post("/{agent_id}/exec")
async def exec_command(
    agent_id: str,
    command: str,
    cwd: str | None = None,
    env: dict[str, str] | None = None,
    timeout_seconds: int = 300,
    token: TokenData = Depends(require_auth),
) -> dict[str, str]:
    """
    Execute a command in the agent's working directory.

    Sends agent_exec intent to the daemon.
    Requires authentication with agent:write scope.
    """
    if not db or not ws_manager:
        raise HTTPException(status_code=503, detail="Service not available")

    async with db.get_session() as session:
        agent_db = await get_agent_by_id(session, UUID(agent_id))
        if not agent_db:
            raise HTTPException(status_code=404, detail="Agent not found")

        daemon_id = agent_db.daemon_id
        target_cwd = cwd or agent_db.cwd

        try:
            intent = AgentExecIntent(
                agent_id=UUID(agent_id),
                command=command,
                cwd=target_cwd,
                env=env or {},
                timeout_seconds=timeout_seconds,
            )
            await ws_manager.send_control_intent(daemon_id, intent.model_dump(mode="json"))

            return {"status": "sent", "message": f"Exec intent sent to daemon {daemon_id}"}
        except ValueError as e:
            raise HTTPException(status_code=503, detail=str(e)) from e


@router.get("/{agent_id}/scrollback")
async def get_scrollback(agent_id: str, lines: int = 1000) -> dict[str, str]:
    """
    Request scrollback from the agent's ring buffer.

    Sends get_scrollback intent to the daemon.
    """
    if not db or not ws_manager:
        raise HTTPException(status_code=503, detail="Service not available")

    async with db.get_session() as session:
        agent_db = await get_agent_by_id(session, UUID(agent_id))
        if not agent_db:
            raise HTTPException(status_code=404, detail="Agent not found")

        daemon_id = agent_db.daemon_id

        try:
            intent = GetScrollbackIntent(
                agent_id=UUID(agent_id),
                lines=lines,
            )
            await ws_manager.send_control_intent(daemon_id, intent.model_dump(mode="json"))

            return {
                "status": "sent",
                "message": f"Scrollback request sent to daemon {daemon_id}",
            }
        except ValueError as e:
            raise HTTPException(status_code=503, detail=str(e)) from e
