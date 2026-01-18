"""HTTP and WebSocket server for multi-terminal Playwright daemon.

Provides a simple HTTP server for serving the HTML template and WebSocket
endpoints for PTY communication with each terminal.
"""

from __future__ import annotations

import asyncio
import logging
import re
import socket
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, HTTPServer
from threading import Thread
from typing import Any

import websockets

from clud.daemon.html_template import get_html_template
from clud.daemon.terminal_manager import TerminalManager

logger = logging.getLogger(__name__)


class DaemonHTTPHandler(BaseHTTPRequestHandler):
    """HTTP request handler for serving the terminal HTML page.

    Attributes:
        server: Reference to the parent HTTPServer (with port and num_terminals)
    """

    # Note: We access server.ws_port and server.num_terminals which are added
    # by DaemonHTTPServer. The type annotation is needed for mypy/pyright.
    server: DaemonHTTPServer  # type: ignore[assignment]

    def do_GET(self) -> None:
        """Handle GET requests.

        Serves the HTML template at / and returns 404 for other paths.
        """
        if self.path == "/" or self.path == "/index.html":
            self._serve_html()
        else:
            self.send_error(HTTPStatus.NOT_FOUND, "Not Found")

    def _serve_html(self) -> None:
        """Serve the terminal HTML page."""
        try:
            # Get the WebSocket port (same as HTTP port in this implementation)
            ws_port = self.server.ws_port
            num_terminals = self.server.num_terminals

            html = get_html_template(port=ws_port, num_terminals=num_terminals)
            html_bytes = html.encode("utf-8")

            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(html_bytes)))
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(html_bytes)

        except Exception as e:
            logger.error("Error serving HTML: %s", e)
            self.send_error(HTTPStatus.INTERNAL_SERVER_ERROR, str(e))

    def log_message(self, format: str, *args: Any) -> None:
        """Override to use Python logging instead of stderr."""
        logger.debug("HTTP: %s", format % args)


class DaemonHTTPServer(HTTPServer):
    """Extended HTTPServer with additional attributes for daemon operation.

    Attributes:
        ws_port: Port for WebSocket connections
        num_terminals: Number of terminals being served
    """

    def __init__(
        self,
        server_address: tuple[str, int],
        handler_class: type[BaseHTTPRequestHandler],
        ws_port: int,
        num_terminals: int,
    ) -> None:
        """Initialize the HTTP server.

        Args:
            server_address: (host, port) tuple
            handler_class: Request handler class
            ws_port: Port for WebSocket connections
            num_terminals: Number of terminals
        """
        super().__init__(server_address, handler_class)
        self.ws_port = ws_port
        self.num_terminals = num_terminals


class DaemonServer:
    """Combined HTTP and WebSocket server for the multi-terminal daemon.

    Manages the HTTP server for serving HTML and WebSocket server for
    terminal communication, along with the TerminalManager.

    Attributes:
        num_terminals: Number of terminals to create
        http_port: Port for HTTP server
        ws_port: Port for WebSocket server
        terminal_manager: Manager for PTY terminal sessions
    """

    def __init__(self, num_terminals: int = 8) -> None:
        """Initialize the daemon server.

        Args:
            num_terminals: Number of terminals to create (default 8)
        """
        self.num_terminals = num_terminals
        self.http_port: int = 0
        self.ws_port: int = 0

        # Server instances
        self._http_server: DaemonHTTPServer | None = None
        self._http_thread: Thread | None = None
        self._ws_server: Any | None = None  # websockets.WebSocketServer
        self._ws_task: asyncio.Task[None] | None = None

        # Terminal manager
        self.terminal_manager: TerminalManager | None = None

        # Running state
        self._running = False

    @staticmethod
    def find_free_port(start: int = 8000, end: int = 9000) -> int:
        """Find a free port in the given range.

        Args:
            start: Start of port range (inclusive)
            end: End of port range (exclusive)

        Returns:
            An available port number

        Raises:
            RuntimeError: If no free port is found in the range
        """
        for port in range(start, end):
            try:
                with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                    s.bind(("localhost", port))
                    return port
            except OSError:
                continue
        raise RuntimeError(f"No free port found in range {start}-{end}")

    async def start(self) -> tuple[int, int]:
        """Start the HTTP and WebSocket servers.

        Returns:
            Tuple of (http_port, ws_port)

        Raises:
            RuntimeError: If servers fail to start
        """
        if self._running:
            logger.warning("Daemon server already running")
            return (self.http_port, self.ws_port)

        try:
            # Find free ports (use sequential ports for easier debugging)
            self.http_port = self.find_free_port(8000, 9000)
            self.ws_port = self.find_free_port(self.http_port + 1, 9100)

            # Start terminal manager
            self.terminal_manager = TerminalManager(
                num_terminals=self.num_terminals,
            )
            started = self.terminal_manager.start_all()
            if started < self.num_terminals:
                logger.warning(
                    "Only %d/%d terminals started",
                    started,
                    self.num_terminals,
                )

            # Start HTTP server in a thread
            await self._start_http_server()

            # Start WebSocket server
            await self._start_ws_server()

            self._running = True
            logger.info(
                "Daemon server started: HTTP=%d, WS=%d, terminals=%d",
                self.http_port,
                self.ws_port,
                started,
            )
            return (self.http_port, self.ws_port)

        except Exception as e:
            logger.error("Failed to start daemon server: %s", e)
            await self.stop()
            raise RuntimeError(f"Failed to start daemon server: {e}") from e

    async def _start_http_server(self) -> None:
        """Start the HTTP server in a background thread."""
        self._http_server = DaemonHTTPServer(
            ("localhost", self.http_port),
            DaemonHTTPHandler,
            ws_port=self.ws_port,
            num_terminals=self.num_terminals,
        )

        self._http_thread = Thread(
            target=self._http_server.serve_forever,
            daemon=True,
            name="daemon-http-server",
        )
        self._http_thread.start()
        logger.debug("HTTP server started on port %d", self.http_port)

    async def _start_ws_server(self) -> None:
        """Start the WebSocket server."""
        self._ws_server = await websockets.serve(
            self._handle_websocket,
            "localhost",
            self.ws_port,
        )
        logger.debug("WebSocket server started on port %d", self.ws_port)

    async def _handle_websocket(
        self,
        websocket: Any,  # websockets.ServerConnection - incomplete stubs
    ) -> None:
        """Handle incoming WebSocket connections.

        Routes connections to the appropriate terminal based on the path.
        Expected path format: /ws/{terminal_id}

        Args:
            websocket: The WebSocket connection
        """
        # In websockets 15.0+, path is accessed via request.path
        path: str = websocket.request.path
        logger.debug("WebSocket connection to: %s", path)

        # Parse terminal ID from path
        match = re.match(r"/ws/(\d+)", path)
        if not match:
            logger.warning("Invalid WebSocket path: %s", path)
            await websocket.close(1003, "Invalid path")
            return

        terminal_id = int(match.group(1))

        # Get terminal
        if self.terminal_manager is None:
            logger.error("Terminal manager not initialized")
            await websocket.close(1011, "Server error")
            return

        terminal = self.terminal_manager.get_terminal(terminal_id)
        if terminal is None:
            logger.warning("Terminal %d not found", terminal_id)
            await websocket.close(1003, "Terminal not found")
            return

        # Handle WebSocket communication
        await terminal.handle_websocket(websocket)

    async def stop(self) -> None:
        """Stop all servers and clean up resources."""
        self._running = False

        # Stop WebSocket server
        if self._ws_server is not None:
            self._ws_server.close()
            await self._ws_server.wait_closed()
            self._ws_server = None
            logger.debug("WebSocket server stopped")

        # Stop HTTP server
        if self._http_server is not None:
            self._http_server.shutdown()
            self._http_server = None
            if self._http_thread is not None:
                self._http_thread.join(timeout=5.0)
                self._http_thread = None
            logger.debug("HTTP server stopped")

        # Stop terminal manager
        if self.terminal_manager is not None:
            self.terminal_manager.stop_all()
            self.terminal_manager = None
            logger.debug("Terminal manager stopped")

        logger.info("Daemon server stopped")

    async def wait_for_close(self) -> None:
        """Wait for the server to be closed.

        This method blocks until stop() is called or the WebSocket server
        is closed externally.
        """
        if self._ws_server is not None:
            await self._ws_server.wait_closed()

    def is_running(self) -> bool:
        """Check if the server is running.

        Returns:
            True if server is running, False otherwise
        """
        return self._running

    def get_url(self) -> str:
        """Get the URL to access the terminal UI.

        Returns:
            HTTP URL to the terminal page
        """
        return f"http://localhost:{self.http_port}/"
