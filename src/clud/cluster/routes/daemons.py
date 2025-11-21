"""
Daemon management routes for CLUD-CLUSTER.

Provides REST API endpoints for:
- Listing connected daemons
- Getting daemon details
"""

from typing import TYPE_CHECKING

from fastapi import APIRouter, HTTPException

from ..models import Daemon, DaemonStatus

if TYPE_CHECKING:
    from ..database import Database
    from ..websocket_handlers import WebSocketConnectionManager

# Module-level dependencies (initialized in init_daemons_routes)
_db: "Database | None" = None
_ws_manager: "WebSocketConnectionManager | None" = None

# Create router
router = APIRouter(prefix="/api/v1/daemons", tags=["daemons"])


def init_daemons_routes(db: "Database", ws_manager: "WebSocketConnectionManager") -> None:
    """
    Initialize daemon routes with dependencies.

    Args:
        db: Database instance
        ws_manager: WebSocket connection manager
    """
    global _db, _ws_manager
    _db = db
    _ws_manager = ws_manager


@router.get("", response_model=list[Daemon])
async def list_daemons() -> list[Daemon]:
    """List all connected daemons."""
    if not _db:
        raise HTTPException(status_code=503, detail="Database not available")

    from ..database import list_daemons as db_list_daemons

    async with _db.get_session() as session:
        daemons_db = await db_list_daemons(session)

        return [
            Daemon(
                id=daemon.id,
                hostname=daemon.hostname,
                platform=daemon.platform,
                version=daemon.version,
                bind_address=daemon.bind_address,
                status=DaemonStatus(daemon.status),
                agent_count=daemon.agent_count,
                created_at=daemon.created_at,
                last_seen=daemon.last_seen,
            )
            for daemon in daemons_db
        ]


@router.get("/{daemon_id}", response_model=Daemon)
async def get_daemon(daemon_id: str) -> Daemon:
    """Get daemon details by ID."""
    if not _db:
        raise HTTPException(status_code=503, detail="Database not available")

    from uuid import UUID

    from ..database import get_daemon_by_id

    async with _db.get_session() as session:
        daemon_db = await get_daemon_by_id(session, UUID(daemon_id))
        if not daemon_db:
            raise HTTPException(status_code=404, detail="Daemon not found")

        return Daemon(
            id=daemon_db.id,
            hostname=daemon_db.hostname,
            platform=daemon_db.platform,
            version=daemon_db.version,
            bind_address=daemon_db.bind_address,
            status=DaemonStatus(daemon_db.status),
            agent_count=daemon_db.agent_count,
            created_at=daemon_db.created_at,
            last_seen=daemon_db.last_seen,
        )
