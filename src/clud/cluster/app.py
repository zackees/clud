"""
Main FastAPI application for CLUD-CLUSTER.

Provides:
- REST API for agent management
- WebSocket endpoints for daemon connections and browser terminals
- Health check and metrics endpoints
"""

import logging
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, HTTPException, Request, WebSocket
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles

from .config import settings
from .database import Database
from .models import BindingMode, Session, SessionType, TelegramBinding
from .routes.agents import router as agents_router
from .routes.daemons import router as daemons_router

# Initialize logging
logging.basicConfig(
    level=getattr(logging, settings.log_level.upper()),
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
)
logger = logging.getLogger(__name__)

# Global database instance
db: Database | None = None

# Global WebSocket connection manager
ws_manager = None

# Global Telegram bot
telegram_bot = None


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    """
    Application lifespan manager.

    Handles startup and shutdown tasks:
    - Database initialization
    - WebSocket connection pool
    - Telegram bot (optional)
    - Background tasks
    """
    global db, ws_manager, telegram_bot
    logger.info(f"Starting {settings.app_name} v{settings.app_version}")
    logger.info(f"Database: {settings.database_url}")

    # Initialize database
    db = Database(settings.database_url)
    await db.create_tables()
    logger.info("Database tables created/verified")

    # Initialize WebSocket connection manager
    from .websocket_handlers import WebSocketConnectionManager

    ws_manager = WebSocketConnectionManager(db.get_session)
    logger.info("WebSocket connection manager initialized")

    # Initialize route modules with dependencies
    from .routes.agents import init_agents_routes
    from .routes.daemons import init_daemons_routes

    init_agents_routes(db, ws_manager)
    logger.info("Agent routes initialized")

    init_daemons_routes(db, ws_manager)
    logger.info("Daemon routes initialized")

    # Initialize Telegram bot (optional)
    if settings.telegram_bot_token:
        from .telegram_bot import TelegramBot

        telegram_bot = TelegramBot(db, settings.telegram_bot_token)
        await telegram_bot.start()
        logger.info("Telegram bot started")
    else:
        logger.info("Telegram bot disabled (no token configured)")

    yield

    # Cleanup
    if telegram_bot:
        await telegram_bot.stop()
    if db:
        await db.close()
    logger.info(f"{settings.app_name} shutdown complete")


# Create FastAPI app
app = FastAPI(
    title=settings.app_name,
    version=settings.app_version,
    description="Cluster control plane for clud agents",
    lifespan=lifespan,
    debug=settings.debug,
)

# CORS middleware for web UI
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],  # TODO: Restrict in production
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Mount static files for web UI (if directory exists)
static_dir = Path(__file__).parent / "static"
if static_dir.exists():
    app.mount("/static", StaticFiles(directory=str(static_dir)), name="static")
    logger.info(f"Serving static files from {static_dir}")
else:
    logger.warning(f"Static directory not found: {static_dir}")

# Include route modules
app.include_router(agents_router)
app.include_router(daemons_router)


# Health check endpoint
@app.get("/health")
async def health_check() -> dict[str, str]:
    """
    Health check endpoint.

    Returns 200 OK if the service is healthy, with basic status info.
    """
    return {
        "status": "healthy",
        "version": settings.app_version,
        "database": "connected" if db else "disconnected",
    }


@app.get("/")
async def root() -> FileResponse | dict[str, str]:
    """Root endpoint - serves web UI if available, otherwise API info."""
    index_file = static_dir / "index.html"
    if index_file.exists():
        return FileResponse(str(index_file))

    # Fallback: API information
    return {
        "service": settings.app_name,
        "version": settings.app_version,
        "docs": "/docs",
        "health": "/health",
    }


# API endpoints (agent routes now in routes/agents.py, daemon routes in routes/daemons.py)


# Authentication endpoints


@app.post("/api/v1/auth/login")
async def login(username: str, password: str) -> dict[str, str | list[str]]:
    """
    Login with username and password.

    Returns a JWT token for authentication.

    NOTE: This is a simplified auth endpoint. In production, use proper
    authentication with hashed passwords stored in database.
    """
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    from .auth import SCOPES_OPERATOR, create_session
    from .database import create_session as db_create_session

    # TODO: Implement proper user authentication with database
    # For now, accept any username/password and create a session
    if not username or not password:
        raise HTTPException(status_code=401, detail="Invalid credentials")

    # Create session
    session = create_session(
        operator_id=username,
        session_type=SessionType.WEB,
        scopes=SCOPES_OPERATOR,
    )

    # Save to database
    async with db.get_session() as db_session:
        await db_create_session(db_session, session)

    return {
        "access_token": session.token,
        "token_type": "bearer",
        "expires_at": session.expires_at.isoformat(),
        "scopes": session.scopes,
    }


@app.post("/api/v1/auth/api-key")
async def create_api_key(operator_id: str, scopes: list[str] | None = None) -> dict[str, str | list[str]]:
    """
    Create an API key for programmatic access.

    Requires admin privileges (TODO: add authentication check).
    """
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    from datetime import timedelta

    from .auth import SCOPES_OPERATOR, create_session
    from .database import create_session as db_create_session

    # Create long-lived API session (30 days)
    session = create_session(
        operator_id=operator_id,
        session_type=SessionType.API,
        scopes=scopes or SCOPES_OPERATOR,
    )
    session.expires_at = session.expires_at + timedelta(days=29)  # 30 days total

    # Save to database
    async with db.get_session() as db_session:
        await db_create_session(db_session, session)

    return {
        "api_key": session.token,
        "operator_id": session.operator_id,
        "expires_at": session.expires_at.isoformat(),
        "scopes": session.scopes,
        "note": "Store this API key securely - it won't be shown again",
    }


@app.get("/api/v1/auth/sessions")
async def list_sessions(operator_id: str | None = None) -> list[Session]:
    """
    List active sessions.

    Query parameters:
    - operator_id: Optional filter by operator
    """
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    from .database import list_sessions as db_list_sessions
    from .models import Session

    async with db.get_session() as db_session:
        sessions_db = await db_list_sessions(db_session, operator_id)

        return [
            Session(
                id=s.id,
                operator_id=s.operator_id,
                type=SessionType(s.type),
                token="***",  # Don't expose tokens
                expires_at=s.expires_at,
                scopes=s.scopes,
            )
            for s in sessions_db
        ]


@app.delete("/api/v1/auth/sessions/{session_id}")
async def revoke_session(session_id: str) -> dict[str, str]:
    """Revoke (delete) a session."""
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    from uuid import UUID

    from .database import delete_session, get_session_by_id

    async with db.get_session() as db_session:
        session_uuid = UUID(session_id)
        existing = await get_session_by_id(db_session, session_uuid)
        if not existing:
            raise HTTPException(status_code=404, detail="Session not found")

        await delete_session(db_session, session_uuid)
        return {"status": "revoked", "session_id": session_id}


# Telegram binding endpoints


@app.get("/api/v1/telegram/bindings")
async def list_telegram_bindings(agent_id: str | None = None, chat_id: int | None = None) -> list[TelegramBinding]:
    """
    List Telegram bindings.

    Query parameters:
    - agent_id: Optional filter by agent ID
    - chat_id: Optional filter by Telegram chat ID
    """
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    from uuid import UUID

    from .database import list_telegram_bindings as db_list_bindings
    from .models import TelegramBinding

    async with db.get_session() as db_session:
        agent_uuid = UUID(agent_id) if agent_id else None
        bindings_db = await db_list_bindings(db_session, agent_uuid, chat_id)

        return [
            TelegramBinding(
                id=b.id,
                chat_id=b.chat_id,
                agent_id=b.agent_id,
                operator_id=b.operator_id,
                mode=BindingMode(b.mode),
                created_at=b.created_at,
            )
            for b in bindings_db
        ]


@app.delete("/api/v1/telegram/bindings/{binding_id}")
async def delete_telegram_binding(binding_id: str) -> dict[str, str]:
    """Delete a Telegram binding."""
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")

    from uuid import UUID

    from .database import delete_telegram_binding as db_delete_binding
    from .database import get_telegram_binding_by_id

    async with db.get_session() as db_session:
        binding_uuid = UUID(binding_id)
        existing = await get_telegram_binding_by_id(db_session, binding_uuid)
        if not existing:
            raise HTTPException(status_code=404, detail="Binding not found")

        await db_delete_binding(db_session, binding_uuid)
        return {"status": "deleted", "binding_id": binding_id}


# Control endpoints (now in routes/agents.py)


# WebSocket endpoints (handlers will be in separate modules)


@app.websocket("/ws/daemon/{daemon_id}")
async def websocket_daemon_control(websocket: WebSocket, daemon_id: str) -> None:
    """
    WebSocket control connection for daemon.

    Handles:
    - Daemon registration
    - Heartbeats
    - Control intents (agent_stop, agent_exec, etc.)
    - Agent lifecycle events
    """
    if not ws_manager:
        await websocket.close(code=1011, reason="Server not ready")
        return

    await ws_manager.handle_daemon_control(websocket, daemon_id)


@app.websocket("/ws/pty/pool-{pool_id}")
async def websocket_pty_pool(websocket: WebSocket, pool_id: str) -> None:
    """
    WebSocket PTY data connection (pooled).

    Handles binary PTY frames with agent_id header.
    Each connection serves 5-10 agents.
    """
    if not ws_manager:
        await websocket.close(code=1011, reason="Server not ready")
        return

    await ws_manager.handle_pty_pool(websocket, pool_id)


@app.websocket("/ws/terminal/{agent_id}")
async def websocket_terminal(websocket: WebSocket, agent_id: str) -> None:
    """
    WebSocket terminal connection for browser.

    Routes PTY data from daemon to browser xterm.js instance.
    """
    if not ws_manager:
        await websocket.close(code=1011, reason="Server not ready")
        return

    await ws_manager.handle_browser_terminal(websocket, agent_id)


@app.websocket("/ws/events")
async def websocket_events(websocket: WebSocket) -> None:
    """
    WebSocket events connection for browser.

    Broadcasts real-time events to UI:
    - agent_updated (status, metrics changes)
    - agent_register (new agent)
    - agent_stopped (agent exited)
    - daemon_connected (daemon online)
    - daemon_disconnected (daemon offline)
    """
    if not ws_manager:
        await websocket.close(code=1011, reason="Server not ready")
        return

    await ws_manager.handle_events(websocket)


# Error handlers


@app.exception_handler(HTTPException)
async def http_exception_handler(request: Request, exc: HTTPException) -> JSONResponse:
    """Handle HTTP exceptions with consistent error format."""
    return JSONResponse(
        status_code=exc.status_code,
        content={
            "error": {
                "code": f"HTTP_{exc.status_code}",
                "message": exc.detail,
                "timestamp": None,  # TODO: Add timestamp
            }
        },
    )


@app.exception_handler(Exception)
async def general_exception_handler(request: Request, exc: Exception) -> JSONResponse:
    """Handle unexpected exceptions."""
    logger.exception("Unhandled exception", exc_info=exc)
    return JSONResponse(
        status_code=500,
        content={
            "error": {
                "code": "INTERNAL_SERVER_ERROR",
                "message": "An unexpected error occurred",
                "timestamp": None,
            }
        },
    )
