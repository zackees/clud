"""HTTP and WebSocket server for Claude Code Web UI."""

import asyncio
import json
import logging
import os
import socket
import sys
import threading
import time
import webbrowser
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
from typing import Any

import websockets.asyncio.server
from websockets.asyncio.server import ServerConnection

from .api import ChatHandler

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


class WebUIRequestHandler(SimpleHTTPRequestHandler):
    """Custom request handler for serving static files."""

    def log_message(self, format: str, *args: Any) -> None:
        """Override to use logger instead of stderr."""
        logger.info("%s - %s", self.address_string(), format % args)

    def end_headers(self) -> None:
        """Add CORS headers for API requests."""
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        super().end_headers()

    def do_OPTIONS(self) -> None:
        """Handle CORS preflight requests."""
        self.send_response(200)
        self.end_headers()


class WebSocketHandler:
    """Handle WebSocket connections for real-time chat."""

    def __init__(self) -> None:
        """Initialize WebSocket handler."""
        self.chat_handler = ChatHandler()
        self.active_connections: set[ServerConnection] = set()

    async def handle_connection(self, websocket: ServerConnection) -> None:
        """Handle a WebSocket connection."""
        self.active_connections.add(websocket)
        logger.info("WebSocket client connected: %s", websocket.remote_address)

        try:
            async for message in websocket:
                await self.handle_message(websocket, message)
        except websockets.exceptions.ConnectionClosed:
            logger.info("WebSocket client disconnected: %s", websocket.remote_address)
        finally:
            self.active_connections.discard(websocket)

    async def handle_message(self, websocket: ServerConnection, message: str | bytes) -> None:
        """Handle incoming WebSocket message."""
        try:
            if isinstance(message, bytes):
                message = message.decode("utf-8")

            data = json.loads(message)
            message_type = data.get("type")

            if message_type == "chat":
                # Handle chat message
                user_message = data.get("message", "")
                project_path = data.get("project_path", os.getcwd())

                # Send acknowledgment
                await websocket.send(json.dumps({"type": "ack", "status": "processing"}))

                # Stream response from Claude Code
                async for chunk in self.chat_handler.handle_chat(user_message, project_path):
                    await websocket.send(json.dumps({"type": "chunk", "content": chunk}))

                # Send completion
                await websocket.send(json.dumps({"type": "done"}))

            elif message_type == "ping":
                await websocket.send(json.dumps({"type": "pong"}))

            else:
                await websocket.send(json.dumps({"type": "error", "error": f"Unknown message type: {message_type}"}))

        except json.JSONDecodeError:
            logger.error("Invalid JSON received: %s", message)
            await websocket.send(json.dumps({"type": "error", "error": "Invalid JSON"}))
        except Exception as e:
            logger.exception("Error handling WebSocket message")
            await websocket.send(json.dumps({"type": "error", "error": str(e)}))


def run_http_server(port: int, static_dir: Path) -> None:
    """Run HTTP server for static files in a separate thread."""
    os.chdir(static_dir)
    server = HTTPServer(("localhost", port), WebUIRequestHandler)
    logger.info("HTTP server listening on http://localhost:%d", port)
    server.serve_forever()


async def run_websocket_server(ws_port: int, handler: WebSocketHandler) -> None:
    """Run WebSocket server for real-time communication."""
    async with websockets.asyncio.server.serve(handler.handle_connection, "localhost", ws_port):
        logger.info("WebSocket server listening on ws://localhost:%d", ws_port)
        await asyncio.Future()  # Run forever


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
    """Start HTTP and WebSocket servers for Web UI.

    Args:
        port: HTTP server port (WebSocket will use port+1). If None, auto-detect.

    Returns:
        Exit code (0 for success)
    """
    try:
        # Find available ports
        if port is None:
            http_port = find_available_port(8888)
        else:
            if not is_port_available(port):
                logger.warning("Port %d is not available, finding alternative...", port)
                http_port = find_available_port(port)
            else:
                http_port = port

        ws_port = http_port + 1
        if not is_port_available(ws_port):
            ws_port = find_available_port(ws_port + 1)

        # Get static directory
        static_dir = Path(__file__).parent / "static"
        if not static_dir.exists():
            logger.error("Static directory not found: %s", static_dir)
            print(f"Error: Static directory not found: {static_dir}", file=sys.stderr)
            return 1

        logger.info("Starting Claude Code Web UI...")
        logger.info("HTTP Port: %d, WebSocket Port: %d", http_port, ws_port)

        url = f"http://localhost:{http_port}"
        print("\n🚀 Claude Code Web UI starting...")
        print(f"📁 HTTP: {url}")
        print(f"🔌 WebSocket: ws://localhost:{ws_port}")
        print()

        # Create WebSocket handler
        ws_handler = WebSocketHandler()

        # Start HTTP server in separate thread
        http_thread = threading.Thread(target=run_http_server, args=(http_port, static_dir), daemon=True)
        http_thread.start()

        # Schedule browser opening
        browser_thread = threading.Thread(target=open_browser_delayed, args=(url,), daemon=True)
        browser_thread.start()

        # Run WebSocket server in asyncio event loop
        asyncio.run(run_websocket_server(ws_port, ws_handler))

        return 0

    except KeyboardInterrupt:
        print("\n\nShutting down Web UI server...")
        return 0
    except Exception as e:
        logger.exception("Error running Web UI server")
        print(f"Error: {e}", file=sys.stderr)
        return 1
