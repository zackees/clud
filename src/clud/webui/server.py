"""FastAPI server for Claude Code Web UI."""

import asyncio
import contextlib
import logging
import os
import socket
import sys
import threading
import time
import webbrowser
from pathlib import Path

import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from fastapi.staticfiles import StaticFiles

from .api import ChatHandler, HistoryHandler, ProjectHandler

# Configure logging
logging.basicConfig(level=logging.INFO, format="%(asctime)s - %(name)s - %(levelname)s - %(message)s")
logger = logging.getLogger(__name__)


def is_port_available(port: int) -> bool:
    """Check if a port is available for binding."""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.bind(("localhost", port))
            return True
    except OSError:
        return False


def find_available_port(start_port: int = 8888) -> int:
    """Find an available port starting from start_port."""
    for port_candidate in range(start_port, start_port + 100):
        if is_port_available(port_candidate):
            return port_candidate
    raise RuntimeError(f"No available ports found starting from {start_port}")


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

    @app.websocket("/ws")
    async def websocket_endpoint(websocket: WebSocket) -> None:
        """WebSocket endpoint for real-time chat."""
        await websocket.accept()
        logger.info("WebSocket client connected: %s", websocket.client)

        try:
            while True:
                # Receive message from client
                data = await websocket.receive_json()
                message_type = data.get("type")

                if message_type == "chat":
                    # Handle chat message
                    user_message = data.get("message", "")
                    project_path = data.get("project_path", os.getcwd())

                    # Send acknowledgment
                    await websocket.send_json({"type": "ack", "status": "processing"})

                    # Stream response from Claude Code
                    async for chunk in chat_handler.handle_chat(user_message, project_path):
                        await websocket.send_json({"type": "chunk", "content": chunk})

                    # Send completion
                    await websocket.send_json({"type": "done"})

                elif message_type == "ping":
                    await websocket.send_json({"type": "pong"})

                else:
                    await websocket.send_json({"type": "error", "error": f"Unknown message type: {message_type}"})

        except WebSocketDisconnect:
            logger.info("WebSocket client disconnected: %s", websocket.client)
        except Exception as e:
            logger.exception("Error handling WebSocket connection")
            # Suppress exception if sending error message fails (connection may be closed)
            with contextlib.suppress(Exception):
                await websocket.send_json({"type": "error", "error": str(e)})

    @app.get("/api/projects")
    async def get_projects(base_path: str | None = None) -> JSONResponse:
        """List available projects."""
        projects = project_handler.list_projects(base_path)
        return JSONResponse(content={"projects": projects})

    @app.get("/api/projects/validate")
    async def validate_project(path: str) -> JSONResponse:
        """Validate a project path."""
        is_valid = project_handler.validate_project_path(path)
        return JSONResponse(content={"valid": is_valid, "path": path})

    @app.get("/api/history")
    async def get_history() -> JSONResponse:
        """Get conversation history."""
        history = history_handler.get_history()
        return JSONResponse(content={"history": history})

    @app.post("/api/history")
    async def add_message(data: dict[str, str]) -> JSONResponse:
        """Add a message to history."""
        role = data.get("role", "user")
        content = data.get("content", "")
        history_handler.add_message(role, content)
        return JSONResponse(content={"status": "ok"})

    @app.delete("/api/history")
    async def clear_history() -> JSONResponse:
        """Clear conversation history."""
        history_handler.clear_history()
        return JSONResponse(content={"status": "ok"})

    @app.get("/health")
    async def health_check() -> JSONResponse:
        """Health check endpoint."""
        return JSONResponse(content={"status": "ok"})

    # Mount static files (must be last)
    app.mount("/", StaticFiles(directory=str(static_dir), html=True), name="static")

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

        # Get static directory
        static_dir = Path(__file__).parent / "static"
        if not static_dir.exists():
            logger.error("Static directory not found: %s", static_dir)
            print(f"Error: Static directory not found: {static_dir}", file=sys.stderr)
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
        app = create_app(static_dir)

        # Schedule browser opening
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
        return 0
    except Exception as e:
        logger.exception("Error running Web UI server")
        print(f"Error: {e}", file=sys.stderr)
        return 1
