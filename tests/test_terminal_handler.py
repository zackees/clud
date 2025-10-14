"""Unit tests for terminal handler."""

import asyncio
import contextlib
import os
import unittest
from typing import cast
from unittest.mock import MagicMock, patch

from clud.webui.pty_manager import PTYManager
from clud.webui.terminal_handler import TerminalHandler


class MockWebSocket:
    """Mock WebSocket for testing."""

    def __init__(self) -> None:
        """Initialize mock WebSocket."""
        self.sent_messages: list[dict[str, object]] = []
        self.received_messages: list[dict[str, object]] = []
        self.client = ("127.0.0.1", 12345)
        self._is_accepting = False

    async def accept(self) -> None:
        """Accept the WebSocket connection."""
        self._is_accepting = True

    async def send_json(self, data: dict[str, object]) -> None:
        """Send JSON data."""
        self.sent_messages.append(data)

    async def receive_json(self) -> dict[str, object]:
        """Receive JSON data."""
        if not self.received_messages:
            # Simulate blocking until message arrives
            await asyncio.sleep(0.1)
            raise asyncio.CancelledError()
        return self.received_messages.pop(0)

    def add_received_message(self, msg: dict[str, object]) -> None:
        """Add a message to the received queue."""
        self.received_messages.append(msg)


class TestTerminalHandler(unittest.TestCase):
    """Test TerminalHandler functionality."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.pty_manager = PTYManager()
        self.handler = TerminalHandler(self.pty_manager)

    def tearDown(self) -> None:
        """Clean up after tests."""
        # Close all sessions
        for session_id in list(self.pty_manager.sessions.keys()):
            self.pty_manager.close_session(session_id)

    def test_init(self) -> None:
        """Test TerminalHandler initialization."""
        self.assertIsInstance(self.handler, TerminalHandler)
        self.assertEqual(self.handler.pty_manager, self.pty_manager)

    async def _test_websocket_init_message(self) -> None:
        """Test WebSocket initialization with init message."""
        websocket = MockWebSocket()

        # Add init message to queue
        websocket.add_received_message(
            {
                "type": "init",
                "cwd": os.getcwd(),
                "cols": 80,
                "rows": 24,
            }
        )

        # Mock PTY session creation
        mock_session = MagicMock()
        mock_session.session_id = "test-1"

        with (
            patch.object(self.pty_manager, "create_session", return_value=mock_session),
            contextlib.suppress(asyncio.TimeoutError),
        ):
            # Start handler (will run until cancelled)
            await asyncio.wait_for(self.handler.handle_websocket(websocket, "test-1"), timeout=1.0)  # type: ignore[arg-type]

        # Check that WebSocket was accepted
        self.assertTrue(websocket._is_accepting)

        # Check that ready message was sent
        ready_messages = [msg for msg in websocket.sent_messages if msg.get("type") == "ready"]
        self.assertEqual(len(ready_messages), 1)

    def test_websocket_init_message(self) -> None:
        """Test WebSocket initialization with init message."""
        asyncio.run(self._test_websocket_init_message())

    async def _test_websocket_missing_init(self) -> None:
        """Test WebSocket with missing init message."""
        websocket = MockWebSocket()

        # Add non-init message
        websocket.add_received_message(
            {
                "type": "input",
                "data": "test",
            }
        )

        # Start handler
        with contextlib.suppress(asyncio.TimeoutError):
            await asyncio.wait_for(self.handler.handle_websocket(websocket, "test-2"), timeout=1.0)  # type: ignore[arg-type]

        # Check that error message was sent
        error_messages = [msg for msg in websocket.sent_messages if msg.get("type") == "error"]
        self.assertTrue(len(error_messages) > 0)
        error_str = cast(str, error_messages[0].get("error", ""))
        self.assertIn("init", error_str.lower())

    def test_websocket_missing_init(self) -> None:
        """Test WebSocket with missing init message."""
        asyncio.run(self._test_websocket_missing_init())

    async def _test_websocket_input(self) -> None:
        """Test WebSocket input handling."""
        websocket = MockWebSocket()

        # Add init message
        websocket.add_received_message(
            {
                "type": "init",
                "cwd": os.getcwd(),
                "cols": 80,
                "rows": 24,
            }
        )

        # Add input message
        websocket.add_received_message(
            {
                "type": "input",
                "data": "echo test\n",
            }
        )

        # Mock PTY session
        mock_session = MagicMock()
        mock_session.session_id = "test-3"

        with patch.object(self.pty_manager, "create_session", return_value=mock_session), patch.object(self.pty_manager, "write_input") as mock_write:
            # Start handler
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.handler.handle_websocket(websocket, "test-3"), timeout=1.0)  # type: ignore[arg-type]

            # Check that write_input was called
            mock_write.assert_called()

    def test_websocket_input(self) -> None:
        """Test WebSocket input handling."""
        asyncio.run(self._test_websocket_input())

    async def _test_websocket_resize(self) -> None:
        """Test WebSocket resize handling."""
        websocket = MockWebSocket()

        # Add init message
        websocket.add_received_message(
            {
                "type": "init",
                "cwd": os.getcwd(),
                "cols": 80,
                "rows": 24,
            }
        )

        # Add resize message
        websocket.add_received_message(
            {
                "type": "resize",
                "cols": 100,
                "rows": 30,
            }
        )

        # Mock PTY session
        mock_session = MagicMock()
        mock_session.session_id = "test-4"

        with patch.object(self.pty_manager, "create_session", return_value=mock_session), patch.object(self.pty_manager, "resize") as mock_resize:
            # Start handler
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.handler.handle_websocket(websocket, "test-4"), timeout=1.0)  # type: ignore[arg-type]

            # Check that resize was called
            mock_resize.assert_called()

    def test_websocket_resize(self) -> None:
        """Test WebSocket resize handling."""
        asyncio.run(self._test_websocket_resize())

    def test_handler_with_real_pty_manager(self) -> None:
        """Test that handler can be constructed with real PTY manager."""
        handler = TerminalHandler(self.pty_manager)
        self.assertIsNotNone(handler)
        self.assertEqual(handler.pty_manager, self.pty_manager)


if __name__ == "__main__":
    unittest.main()
