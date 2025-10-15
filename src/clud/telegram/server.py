"""Main server for Telegram integration.

Integrates the Telegram bot handler, WebSocket server, and REST API into a unified service.
"""

import asyncio
import contextlib
import logging
import signal
import sys
import webbrowser
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from fastapi import FastAPI, WebSocket
from fastapi.responses import HTMLResponse
from fastapi.staticfiles import StaticFiles

from clud.api.instance_manager import InstancePool
from clud.telegram.api import create_telegram_api_router
from clud.telegram.bot_handler import TelegramBotHandler
from clud.telegram.config import TelegramIntegrationConfig
from clud.telegram.session_manager import SessionManager
from clud.telegram.ws_server import telegram_websocket_endpoint

logger = logging.getLogger(__name__)


class TelegramServer:
    """Main server for Telegram integration."""

    def __init__(self, config: TelegramIntegrationConfig) -> None:
        """Initialize the Telegram server.

        Args:
            config: Configuration for the Telegram integration
        """
        self.config = config
        self.instance_pool = InstancePool(max_instances=config.sessions.max_sessions, idle_timeout_seconds=config.sessions.timeout_seconds)
        self.session_manager = SessionManager(
            instance_pool=self.instance_pool,
            max_sessions=config.sessions.max_sessions,
            session_timeout_seconds=config.sessions.timeout_seconds,
        )
        self.bot_handler: TelegramBotHandler | None = None
        self.app: FastAPI | None = None
        self._cleanup_task: asyncio.Task[None] | None = None
        self._shutdown_event = asyncio.Event()

    async def start(self) -> None:
        """Start the Telegram server (bot + web interface)."""
        logger.info("Starting Telegram server...")

        # Start cleanup task
        self._cleanup_task = asyncio.create_task(self._cleanup_loop())

        # Start bot handler
        if self.config.telegram.bot_token:
            logger.info("Starting Telegram bot handler...")
            self.bot_handler = TelegramBotHandler(self.config, self.session_manager)
            await self.bot_handler.start_polling()
        else:
            logger.warning("No bot token provided, bot handler will not start")

        # Create FastAPI app
        self.app = await self._create_app()

        logger.info("Telegram server started successfully")

    async def stop(self) -> None:
        """Stop the Telegram server."""
        logger.info("Stopping Telegram server...")

        # Stop bot handler
        if self.bot_handler:
            logger.info("Stopping bot handler...")
            await self.bot_handler.stop()

        # Stop cleanup task
        if self._cleanup_task:
            self._cleanup_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._cleanup_task

        # Clean up all sessions
        logger.info("Cleaning up sessions...")
        sessions = self.session_manager.get_all_sessions()
        for session in sessions:
            await self.session_manager.delete_session(session.session_id)

        # Clean up instance pool
        logger.info("Cleaning up instance pool...")
        await self.instance_pool.shutdown()

        logger.info("Telegram server stopped")

    async def _create_app(self) -> FastAPI:
        """Create the FastAPI application.

        Returns:
            The configured FastAPI app
        """

        @asynccontextmanager
        async def lifespan(app: FastAPI) -> AsyncIterator[None]:
            """Lifespan context manager for FastAPI."""
            # Startup
            logger.info("FastAPI application starting...")
            yield
            # Shutdown
            logger.info("FastAPI application shutting down...")
            await self.stop()

        app = FastAPI(
            title="Claude Code - Telegram Integration",
            description="Web interface for Telegram bot sessions",
            version="1.0.0",
            lifespan=lifespan,
        )

        # Add REST API router
        auth_token = self.config.web.auth_token if self.config.web.auth_required else None
        api_router = create_telegram_api_router(self.session_manager, auth_token)
        app.include_router(api_router)

        # WebSocket endpoint
        @app.websocket("/ws/telegram/{session_id}")
        async def websocket_endpoint(websocket: WebSocket, session_id: str) -> None:
            await telegram_websocket_endpoint(websocket, session_id, self.session_manager, auth_token)

        # Serve SvelteKit frontend
        frontend_build_dir = Path(__file__).parent / "frontend" / "build"

        if frontend_build_dir.exists():
            # Serve static files (JS, CSS, etc.)
            app.mount("/_app", StaticFiles(directory=str(frontend_build_dir / "_app")), name="static")

            # Serve index.html for all other routes (SPA mode)
            @app.get("/{full_path:path}", response_class=HTMLResponse)
            async def serve_spa(full_path: str) -> HTMLResponse:
                # Serve index.html for all routes (SPA mode)
                index_path = frontend_build_dir / "index.html"
                return HTMLResponse(content=index_path.read_text(), status_code=200)
        else:
            # Fallback to landing page if frontend not built
            logger.warning("Frontend build not found, serving landing page")

            @app.get("/", response_class=HTMLResponse)
            async def root() -> HTMLResponse:
                return HTMLResponse(self._get_landing_page_html())

        @app.get("/api/health")
        async def health() -> dict[str, str]:
            return {"status": "healthy"}

        return app

    def _get_landing_page_html(self) -> str:
        """Get the landing page HTML.

        Returns:
            HTML content for the landing page
        """
        bot_username = "clud_ckl_bot"  # TODO: Get from bot API
        return f"""
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Claude Code - Telegram Dashboard</title>
    <style>
        * {{
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
            padding: 20px;
        }}
        .container {{
            background: white;
            border-radius: 20px;
            box-shadow: 0 20px 60px rgba(0, 0, 0, 0.3);
            max-width: 600px;
            width: 100%;
            padding: 40px;
            text-align: center;
        }}
        h1 {{
            font-size: 32px;
            margin-bottom: 10px;
            color: #333;
        }}
        .subtitle {{
            font-size: 18px;
            color: #666;
            margin-bottom: 30px;
        }}
        .status {{
            display: inline-block;
            padding: 10px 20px;
            background: #10b981;
            color: white;
            border-radius: 20px;
            font-weight: 500;
            margin-bottom: 30px;
        }}
        .info-box {{
            background: #f3f4f6;
            border-radius: 10px;
            padding: 20px;
            margin-bottom: 20px;
            text-align: left;
        }}
        .info-box h3 {{
            font-size: 16px;
            color: #333;
            margin-bottom: 10px;
        }}
        .info-box p {{
            font-size: 14px;
            color: #666;
            line-height: 1.6;
        }}
        .btn {{
            display: inline-block;
            padding: 12px 30px;
            background: #667eea;
            color: white;
            text-decoration: none;
            border-radius: 8px;
            font-weight: 500;
            margin: 5px;
            transition: background 0.3s;
        }}
        .btn:hover {{
            background: #5568d3;
        }}
        .btn-secondary {{
            background: #6b7280;
        }}
        .btn-secondary:hover {{
            background: #4b5563;
        }}
        .stats {{
            display: flex;
            justify-content: space-around;
            margin-top: 30px;
            padding-top: 30px;
            border-top: 1px solid #e5e7eb;
        }}
        .stat {{
            text-align: center;
        }}
        .stat-value {{
            font-size: 28px;
            font-weight: 700;
            color: #667eea;
        }}
        .stat-label {{
            font-size: 12px;
            color: #666;
            margin-top: 5px;
            text-transform: uppercase;
            letter-spacing: 0.5px;
        }}
        .footer {{
            margin-top: 30px;
            font-size: 12px;
            color: #999;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>🤖 Claude Code</h1>
        <p class="subtitle">Telegram Dashboard</p>

        <div class="status">● Server Running</div>

        <div class="info-box">
            <h3>📱 Telegram Bot</h3>
            <p>Chat with Claude Code on Telegram. Your conversations will appear here in real-time.</p>
        </div>

        <div class="info-box">
            <h3>🌐 Web Dashboard</h3>
            <p>This interface will display all active Telegram sessions and allow you to monitor conversations in real-time.</p>
        </div>

        <div style="margin-top: 20px;">
            <a href="https://t.me/{bot_username}" class="btn" target="_blank">Open Telegram Bot</a>
            <a href="/api/telegram/sessions" class="btn btn-secondary" target="_blank">View API</a>
        </div>

        <div class="stats" id="stats">
            <div class="stat">
                <div class="stat-value" id="total-sessions">-</div>
                <div class="stat-label">Total Sessions</div>
            </div>
            <div class="stat">
                <div class="stat-value" id="active-sessions">-</div>
                <div class="stat-label">Active Now</div>
            </div>
            <div class="stat">
                <div class="stat-value" id="total-messages">-</div>
                <div class="stat-label">Messages</div>
            </div>
        </div>

        <div class="footer">
            Phase 2 will bring the full SvelteKit dashboard with real-time chat interface.
        </div>
    </div>

    <script>
        // Fetch and display stats
        async function updateStats() {{
            try {{
                const response = await fetch('/api/telegram/health');
                const data = await response.json();
                document.getElementById('total-sessions').textContent = data.total_sessions;
                document.getElementById('active-sessions').textContent = data.active_sessions;
                document.getElementById('total-messages').textContent = data.total_messages;
            }} catch (e) {{
                console.error('Failed to fetch stats:', e);
            }}
        }}

        // Update stats every 5 seconds
        updateStats();
        setInterval(updateStats, 5000);
    </script>
</body>
</html>
"""

    async def _cleanup_loop(self) -> None:
        """Periodic cleanup task for idle sessions."""
        logger.info("Starting cleanup loop...")
        cleanup_interval = self.config.sessions.cleanup_interval

        while True:
            try:
                await asyncio.sleep(cleanup_interval)
                logger.debug("Running session cleanup...")
                await self.session_manager.cleanup_idle_sessions()
            except asyncio.CancelledError:
                logger.info("Cleanup loop cancelled")
                break
            except Exception as e:
                logger.error(f"Error in cleanup loop: {e}", exc_info=True)


async def run_telegram_server(config: TelegramIntegrationConfig, open_browser: bool = True) -> None:
    """Run the Telegram server.

    Args:
        config: Configuration for the Telegram integration
        open_browser: Whether to open the browser after starting
    """
    import uvicorn

    server = TelegramServer(config)

    # Set up signal handlers for graceful shutdown
    def signal_handler(sig: int, frame: Any) -> None:
        logger.info(f"Received signal {sig}, shutting down...")
        asyncio.create_task(server.stop())
        sys.exit(0)

    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)

    # Start the server
    await server.start()

    # Open browser
    if open_browser:
        url = f"http://{config.web.host}:{config.web.port}"
        logger.info(f"Opening browser at {url}")
        await asyncio.sleep(2)  # Wait for server to be ready
        webbrowser.open(url)

    # Run uvicorn server
    if server.app:
        uvicorn_config = uvicorn.Config(
            server.app,
            host=config.web.host,
            port=config.web.port,
            log_level=config.logging.level.lower(),
        )
        uvicorn_server = uvicorn.Server(uvicorn_config)
        await uvicorn_server.serve()
