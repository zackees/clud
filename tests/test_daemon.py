"""Unit tests for the daemon module.

Tests for DaemonInfo, Terminal, TerminalManager, DaemonServer, and HTML template
generation.
"""

from __future__ import annotations

import socket
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch


class TestDaemonInfoDataclass(unittest.TestCase):
    """Test cases for DaemonInfo dataclass."""

    def test_daemon_info_creation(self) -> None:
        """Test creating a DaemonInfo instance."""
        from clud.daemon import DaemonInfo

        info = DaemonInfo(pid=12345, port=8000, num_terminals=8)

        self.assertEqual(info.pid, 12345)
        self.assertEqual(info.port, 8000)
        self.assertEqual(info.num_terminals, 8)

    def test_daemon_info_default_values(self) -> None:
        """Test DaemonInfo with different values."""
        from clud.daemon import DaemonInfo

        info = DaemonInfo(pid=1, port=9999, num_terminals=4)

        self.assertEqual(info.pid, 1)
        self.assertEqual(info.port, 9999)
        self.assertEqual(info.num_terminals, 4)

    def test_daemon_info_is_dataclass(self) -> None:
        """Test that DaemonInfo is a proper dataclass."""
        from dataclasses import fields

        from clud.daemon import DaemonInfo

        # Check that it's a dataclass with expected fields
        field_names = [f.name for f in fields(DaemonInfo)]
        self.assertIn("pid", field_names)
        self.assertIn("port", field_names)
        self.assertIn("num_terminals", field_names)


class TestTerminalCreation(unittest.TestCase):
    """Test cases for Terminal class initialization."""

    def test_terminal_creation_default_cwd(self) -> None:
        """Test creating a Terminal with default cwd."""
        from clud.daemon.terminal_manager import Terminal

        terminal = Terminal(terminal_id=0)

        self.assertEqual(terminal.terminal_id, 0)
        self.assertEqual(terminal.cwd, str(Path.home()))
        self.assertFalse(terminal.is_running)

    def test_terminal_creation_custom_cwd(self) -> None:
        """Test creating a Terminal with custom cwd."""
        from clud.daemon.terminal_manager import Terminal

        custom_cwd = "/tmp/test"
        terminal = Terminal(terminal_id=5, cwd=custom_cwd)

        self.assertEqual(terminal.terminal_id, 5)
        self.assertEqual(terminal.cwd, custom_cwd)
        self.assertFalse(terminal.is_running)

    def test_terminal_initial_state(self) -> None:
        """Test Terminal initial state."""
        from clud.daemon.terminal_manager import Terminal

        terminal = Terminal(terminal_id=3)

        self.assertIsNone(terminal._pty_process)
        self.assertIsNone(terminal._pty_fd)
        self.assertIsNone(terminal._read_task)
        self.assertIsNone(terminal._websocket)
        self.assertEqual(terminal._cols, 80)
        self.assertEqual(terminal._rows, 24)

    def test_terminal_stop_when_not_running(self) -> None:
        """Test that stopping a non-running terminal is safe."""
        from clud.daemon.terminal_manager import Terminal

        terminal = Terminal(terminal_id=0)
        # This should not raise any errors
        terminal.stop()
        self.assertFalse(terminal.is_running)


class TestTerminalManagerCreation(unittest.TestCase):
    """Test cases for TerminalManager class initialization."""

    def test_terminal_manager_creation_default(self) -> None:
        """Test creating a TerminalManager with default values."""
        from clud.daemon.terminal_manager import TerminalManager

        manager = TerminalManager()

        self.assertEqual(manager.num_terminals, 8)
        self.assertEqual(manager.cwd, str(Path.home()))
        self.assertEqual(len(manager.terminals), 0)

    def test_terminal_manager_creation_custom(self) -> None:
        """Test creating a TerminalManager with custom values."""
        from clud.daemon.terminal_manager import TerminalManager

        manager = TerminalManager(num_terminals=4, cwd="/tmp/test")

        self.assertEqual(manager.num_terminals, 4)
        self.assertEqual(manager.cwd, "/tmp/test")
        self.assertEqual(len(manager.terminals), 0)

    def test_terminal_manager_get_terminal_empty(self) -> None:
        """Test getting a terminal from empty manager."""
        from clud.daemon.terminal_manager import TerminalManager

        manager = TerminalManager()

        self.assertIsNone(manager.get_terminal(0))
        self.assertIsNone(manager.get_terminal(7))
        self.assertIsNone(manager.get_terminal(99))

    def test_terminal_manager_is_all_running_empty(self) -> None:
        """Test is_all_running with empty manager."""
        from clud.daemon.terminal_manager import TerminalManager

        manager = TerminalManager()

        # No terminals started yet, so not all running
        self.assertFalse(manager.is_all_running())

    def test_terminal_manager_get_running_count_empty(self) -> None:
        """Test get_running_count with empty manager."""
        from clud.daemon.terminal_manager import TerminalManager

        manager = TerminalManager()

        self.assertEqual(manager.get_running_count(), 0)

    def test_terminal_manager_stop_all_empty(self) -> None:
        """Test stopping all terminals when none are running."""
        from clud.daemon.terminal_manager import TerminalManager

        manager = TerminalManager()
        # This should not raise any errors
        manager.stop_all()
        self.assertEqual(len(manager.terminals), 0)


class TestFindFreePort(unittest.TestCase):
    """Test cases for DaemonServer.find_free_port() method."""

    def test_find_free_port_returns_valid_port(self) -> None:
        """Test that find_free_port returns a valid port number."""
        from clud.daemon.server import DaemonServer

        port = DaemonServer.find_free_port(start=10000, end=10100)

        self.assertIsInstance(port, int)
        self.assertGreaterEqual(port, 10000)
        self.assertLess(port, 10100)

    def test_find_free_port_is_actually_free(self) -> None:
        """Test that the returned port is actually free."""
        from clud.daemon.server import DaemonServer

        port = DaemonServer.find_free_port(start=10000, end=10100)

        # Try to bind to the port - should succeed
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("localhost", port))
            # If we get here, the port was free

    def test_find_free_port_no_available_raises(self) -> None:
        """Test that find_free_port raises when no ports available."""
        from clud.daemon.server import DaemonServer

        # Bind to all ports in a small range
        sockets: list[socket.socket] = []
        start_port = 10200
        end_port = 10205

        try:
            for port in range(start_port, end_port):
                sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                try:
                    sock.bind(("localhost", port))
                    sockets.append(sock)
                except OSError:
                    # Port already in use, skip
                    sock.close()

            # Now try to find a free port in that range
            with self.assertRaises(RuntimeError) as ctx:
                DaemonServer.find_free_port(start=start_port, end=end_port)

            self.assertIn("No free port found", str(ctx.exception))

        finally:
            # Clean up sockets
            for sock in sockets:
                sock.close()

    def test_find_free_port_default_range(self) -> None:
        """Test find_free_port with default range."""
        from clud.daemon.server import DaemonServer

        port = DaemonServer.find_free_port()

        self.assertIsInstance(port, int)
        self.assertGreaterEqual(port, 8000)
        self.assertLess(port, 9000)


class TestHtmlTemplateGeneration(unittest.TestCase):
    """Test cases for HTML template generation."""

    def test_html_template_basic_structure(self) -> None:
        """Test that HTML template has basic structure."""
        from clud.daemon.html_template import get_html_template

        html = get_html_template(port=8001, num_terminals=8)

        self.assertIn("<!DOCTYPE html>", html)
        self.assertIn("<html", html)
        self.assertIn("</html>", html)
        self.assertIn("<head>", html)
        self.assertIn("</head>", html)
        self.assertIn("<body>", html)
        self.assertIn("</body>", html)

    def test_html_template_title(self) -> None:
        """Test that HTML template has correct title."""
        from clud.daemon.html_template import get_html_template

        html = get_html_template(port=8001, num_terminals=8)

        self.assertIn("<title>CLUD Multi-Terminal</title>", html)

    def test_html_template_xterm_scripts(self) -> None:
        """Test that HTML template includes xterm.js scripts."""
        from clud.daemon.html_template import get_html_template

        html = get_html_template(port=8001, num_terminals=8)

        self.assertIn("xterm@5.3.0", html)
        self.assertIn("xterm-addon-fit@0.8.0", html)
        self.assertIn("xterm-addon-web-links@0.9.0", html)

    def test_html_template_terminal_divs(self) -> None:
        """Test that HTML template has correct number of terminal divs."""
        from clud.daemon.html_template import get_html_template

        html = get_html_template(port=8001, num_terminals=8)

        for i in range(8):
            self.assertIn(f'id="terminal-{i}"', html)

    def test_html_template_websocket_urls(self) -> None:
        """Test that HTML template has correct WebSocket URLs."""
        from clud.daemon.html_template import get_html_template

        html = get_html_template(port=9001, num_terminals=4)

        for i in range(4):
            self.assertIn(f"ws://localhost:9001/ws/{i}", html)

    def test_html_template_custom_terminal_count(self) -> None:
        """Test HTML template with custom terminal count."""
        from clud.daemon.html_template import get_html_template

        html = get_html_template(port=8001, num_terminals=4)

        # Should have terminals 0-3
        for i in range(4):
            self.assertIn(f'id="terminal-{i}"', html)

        # Should NOT have terminal 4
        self.assertNotIn('id="terminal-4"', html)

    def test_html_template_grid_layout(self) -> None:
        """Test that HTML template uses grid layout."""
        from clud.daemon.html_template import get_html_template

        html = get_html_template(port=8001, num_terminals=8)

        self.assertIn("display: grid", html)
        self.assertIn("grid-template-columns", html)
        self.assertIn("grid-template-rows", html)

    def test_minimal_html_template(self) -> None:
        """Test minimal HTML template for testing."""
        from clud.daemon.html_template import get_minimal_html_template

        html = get_minimal_html_template(port=8001, num_terminals=4)

        self.assertIn("<!DOCTYPE html>", html)
        self.assertIn("CLUD Multi-Terminal (Minimal)", html)

        for i in range(4):
            self.assertIn(f'id="terminal-{i}"', html)
            self.assertIn(f"ws://localhost:8001/ws/{i}", html)


class TestDaemonServerCreation(unittest.TestCase):
    """Test cases for DaemonServer class initialization."""

    def test_daemon_server_creation_default(self) -> None:
        """Test creating a DaemonServer with default values."""
        from clud.daemon.server import DaemonServer

        server = DaemonServer()

        self.assertEqual(server.num_terminals, 8)
        self.assertEqual(server.http_port, 0)
        self.assertEqual(server.ws_port, 0)
        self.assertIsNone(server.terminal_manager)
        self.assertFalse(server.is_running())

    def test_daemon_server_creation_custom(self) -> None:
        """Test creating a DaemonServer with custom values."""
        from clud.daemon.server import DaemonServer

        server = DaemonServer(num_terminals=4)

        self.assertEqual(server.num_terminals, 4)

    def test_daemon_server_get_url_initial(self) -> None:
        """Test get_url returns correct format."""
        from clud.daemon.server import DaemonServer

        server = DaemonServer()
        # Port is 0 initially
        url = server.get_url()

        self.assertEqual(url, "http://localhost:0/")


class TestDaemonHTTPHandler(unittest.TestCase):
    """Test cases for DaemonHTTPHandler."""

    def test_http_handler_serves_root(self) -> None:
        """Test that HTTP handler serves HTML at root."""
        from clud.daemon.server import DaemonHTTPHandler, DaemonHTTPServer

        # Create a mock server with required attributes
        mock_server = MagicMock(spec=DaemonHTTPServer)
        mock_server.ws_port = 8001
        mock_server.num_terminals = 8

        # Create handler with mock request and client
        handler = DaemonHTTPHandler.__new__(DaemonHTTPHandler)
        handler.server = mock_server
        handler.path = "/"
        handler.requestline = "GET / HTTP/1.1"
        handler.client_address = ("127.0.0.1", 12345)
        handler.request_version = "HTTP/1.1"
        handler.command = "GET"

        # Mock the response methods
        handler.send_response = MagicMock()
        handler.send_header = MagicMock()
        handler.end_headers = MagicMock()
        handler.wfile = MagicMock()

        # Call do_GET
        handler.do_GET()

        # Verify response was sent
        handler.send_response.assert_called_once_with(200)


class TestDaemonProxyClass(unittest.TestCase):
    """Test cases for the Daemon proxy class."""

    def test_daemon_proxy_is_running_no_pid_file(self) -> None:
        """Test is_running returns False when no PID file."""
        from clud.daemon import Daemon

        # Mock PID file not existing
        with patch("clud.daemon.playwright_daemon._PID_FILE") as mock_pid_file:
            mock_pid_file.exists.return_value = False
            result = Daemon.is_running()

        self.assertFalse(result)


class TestPlaywrightDaemon(unittest.TestCase):
    """Test cases for PlaywrightDaemon class."""

    def test_playwright_daemon_creation(self) -> None:
        """Test creating a PlaywrightDaemon instance."""
        from clud.daemon.playwright_daemon import PlaywrightDaemon

        daemon = PlaywrightDaemon(num_terminals=4)

        self.assertEqual(daemon.num_terminals, 4)
        self.assertIsNone(daemon._port)
        self.assertIsNone(daemon._pid)
        self.assertIsNone(daemon._playwright)
        self.assertIsNone(daemon._browser)
        self.assertIsNone(daemon._context)
        self.assertIsNone(daemon._page)
        self.assertIsNone(daemon._server)
        self.assertIsNone(daemon._closed_event)

    def test_playwright_daemon_default_terminals(self) -> None:
        """Test PlaywrightDaemon with default terminal count."""
        from clud.daemon.playwright_daemon import PlaywrightDaemon

        daemon = PlaywrightDaemon()

        self.assertEqual(daemon.num_terminals, 8)

    def test_playwright_daemon_server_property(self) -> None:
        """Test PlaywrightDaemon server property."""
        from clud.daemon.playwright_daemon import PlaywrightDaemon

        daemon = PlaywrightDaemon()

        # Initially None
        self.assertIsNone(daemon.server)


if __name__ == "__main__":
    unittest.main()
