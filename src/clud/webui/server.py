"""FastAPI server for Claude Code Web UI."""

import _thread
import asyncio
import logging
import os
import sys
import threading
import time
import webbrowser
from pathlib import Path

import uvicorn
from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

from .api import BacklogHandler, ChatHandler, DiffHandler, HistoryHandler, ProjectHandler
from .pty_manager import PTYManager
from .rest_routes import register_rest_routes
from .server_config import ensure_frontend_built, find_available_port, is_port_available
from .static_routes import init_mime_types, register_static_routes
from .telegram_api import TelegramAPIHandler
from .terminal_handler import TerminalHandler
from .websocket_routes import register_websocket_routes

# Initialize MIME types for Windows compatibility
init_mime_types()

# Configure logging
logging.basicConfig(level=logging.INFO, format="%(asctime)s - %(name)s - %(levelname)s - %(message)s")
logger = logging.getLogger(__name__)


def create_app(static_dir: Path) -> FastAPI:
    """Create and configure FastAPI application.

    Args:
        static_dir: Directory containing static files

    Returns:
        Configured FastAPI application
    """
    app = FastAPI(
        title="Claude Code Web UI",
        description="Web interface for Claude Code with real-time chat",
        version="1.0.0",
    )

    # Add CORS middleware
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    # Initialize handlers
    chat_handler = ChatHandler()
    project_handler = ProjectHandler()
    history_handler = HistoryHandler()
    diff_handler = DiffHandler()
    backlog_handler = BacklogHandler()
    pty_manager = PTYManager()
    terminal_handler = TerminalHandler(pty_manager)
    telegram_handler = TelegramAPIHandler()

    # Register WebSocket routes
    register_websocket_routes(app, chat_handler, terminal_handler, telegram_handler)

    # Register REST API routes
    register_rest_routes(app, project_handler, history_handler, diff_handler, backlog_handler, telegram_handler)

    # Register static file serving routes (must be last)
    register_static_routes(app, static_dir)

    return app


def open_browser_delayed(url: str, delay: float = 2.0) -> None:
    """Open browser after a delay."""
    time.sleep(delay)
    logger.info("Opening browser to %s", url)
    try:
        webbrowser.open(url)
        print(f"\n✓ Claude Code Web UI is now accessible at {url}")
        print("\nPress Ctrl+C to stop the server")
    except Exception as e:
        logger.error("Could not open browser automatically: %s", e)
        print(f"Please open {url} in your browser")


def run_server(port: int | None = None) -> int:
    """Start FastAPI server for Web UI.

    Args:
        port: Server port. If None, auto-detect.

    Returns:
        Exit code (0 for success)
    """
    try:
        # Find available port
        if port is None:
            http_port = find_available_port(8888)
        else:
            if not is_port_available(port):
                logger.warning("Port %d is not available, finding alternative...", port)
                http_port = find_available_port(port)
            else:
                http_port = port

        # Auto-build frontend if needed (only if source exists)
        ensure_frontend_built()

        # Get static directory - prefer frontend/build over static for Svelte app
        frontend_build_dir = Path(__file__).parent / "frontend" / "build"
        static_dir = Path(__file__).parent / "static"

        if frontend_build_dir.exists():
            # Use new Svelte frontend
            serve_dir = frontend_build_dir
            logger.info("Serving from Svelte build: %s", serve_dir)
        elif static_dir.exists():
            # Fall back to old static files
            serve_dir = static_dir
            logger.info("Serving from legacy static: %s", serve_dir)
        else:
            logger.error("Neither frontend build nor static directory found")
            print("Error: No frontend files found", file=sys.stderr)
            return 1

        logger.info("Starting Claude Code Web UI...")
        logger.info("Port: %d", http_port)

        url = f"http://localhost:{http_port}"
        print("\n🚀 Claude Code Web UI starting...")
        print(f"📁 Server: {url}")
        print(f"🔌 WebSocket: ws://localhost:{http_port}/ws")
        print(f"📚 API Docs: {url}/docs")
        print()

        # Create FastAPI app
        app = create_app(serve_dir)

        # Schedule browser opening (unless disabled for testing)
        if not os.environ.get("CLUD_NO_BROWSER"):
            browser_thread = threading.Thread(target=open_browser_delayed, args=(url,), daemon=True)
            browser_thread.start()

        # Run server with uvicorn
        config = uvicorn.Config(
            app,
            host="localhost",
            port=http_port,
            log_level="info",
            access_log=True,
        )
        server = uvicorn.Server(config)

        # Run in asyncio event loop
        asyncio.run(server.serve())

        return 0

    except KeyboardInterrupt:
        print("\n\nShutting down Web UI server...")
        # Interrupt main thread to ensure proper cleanup
        _thread.interrupt_main()
        return 0
    except Exception as e:
        logger.exception("Error running Web UI server")
        print(f"Error: {e}", file=sys.stderr)
        return 1
