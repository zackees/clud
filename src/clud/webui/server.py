"""FastAPI server for Claude Code Web UI."""

from __future__ import annotations

import _thread
import asyncio
import logging
import os
import sys
import threading
import time
import webbrowser
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fastapi import FastAPI


def create_app(static_dir: Path) -> FastAPI:
    """Create and configure FastAPI application.

    Args:
        static_dir: Directory containing static files

    Returns:
        Configured FastAPI application
    """
    from fastapi import FastAPI
    from fastapi.middleware.cors import CORSMiddleware

    from .api import BacklogHandler, ChatHandler, DiffHandler, HistoryHandler, ProjectHandler
    from .pty_manager import PTYManager
    from .rest_routes import register_rest_routes
    from .static_routes import register_static_routes
    from .telegram_api import TelegramAPIHandler
    from .terminal_handler import TerminalHandler
    from .websocket_routes import register_websocket_routes

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
    logger = logging.getLogger(__name__)
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
    import uvicorn

    from .server_config import find_available_port, get_frontend_build_dir, is_port_available
    from .static_routes import init_mime_types

    # Initialize MIME types for Windows compatibility
    init_mime_types()

    # Configure logging
    logging.basicConfig(level=logging.INFO, format="%(asctime)s - %(name)s - %(levelname)s - %(message)s")
    logger = logging.getLogger(__name__)

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

        # Get frontend build directory (uses global cache with locking)
        frontend_build_dir = get_frontend_build_dir()

        if frontend_build_dir and frontend_build_dir.exists():
            # Use frontend build (from cache or local)
            serve_dir = frontend_build_dir
            logger.info("Serving from frontend build: %s", serve_dir)
        else:
            # Fall back to legacy static files if available
            static_dir = Path(__file__).parent / "static"
            if static_dir.exists():
                serve_dir = static_dir
                logger.info("Serving from legacy static: %s", serve_dir)
            else:
                logger.error("No frontend files available")
                print("Error: No frontend files found", file=sys.stderr)
                print("Please ensure the frontend is built or Node.js is installed for auto-build.", file=sys.stderr)
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
