"""Unit tests for TelegramWebSocketHandler.

Tests the WebSocket handler's connection management, authentication,
message routing, and error handling.
"""

import unittest
from datetime import datetime
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import WebSocket

from clud.telegram.models import ContentType, EventType, MessageSender, TelegramMessage, TelegramSession
from clud.telegram.session_manager import SessionManager
from clud.telegram.ws_server import TelegramWebSocketHandler

pytestmark = pytest.mark.anyio


class TestTelegramWebSocketHandler(unittest.IsolatedAsyncioTestCase):
    """Test cases for TelegramWebSocketHandler."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create mock session manager
        self.session_manager = MagicMock(spec=SessionManager)

        # Create handler
        self.handler = TelegramWebSocketHandler(session_manager=self.session_manager, auth_token=None)

        # Create mock websocket
        self.websocket = AsyncMock(spec=WebSocket)
        self.websocket.accept = AsyncMock()
        self.websocket.close = AsyncMock()
        self.websocket.receive_json = AsyncMock()
        self.websocket.send_json = AsyncMock()

        # Create test session
        self.session = TelegramSession(
            session_id="test-session-123",
            telegram_user_id=12345,
            telegram_username="testuser",
            telegram_first_name="Test",
            telegram_last_name="User",
            instance_id="test-instance",
            message_history=[
                TelegramMessage(
                    message_id="msg1",
                    session_id="test-session-123",
                    telegram_message_id=1,
                    sender=MessageSender.USER,
                    content="Hello",
                    content_type=ContentType.TEXT,
                    timestamp=datetime.now(),
                    metadata={},
                )
            ],
            created_at=datetime.now(),
            last_activity=datetime.now(),
            is_active=True,
        )

    async def test_init(self) -> None:
        """Test handler initialization."""
        handler = TelegramWebSocketHandler(session_manager=self.session_manager, auth_token="secret")

        assert handler.session_manager == self.session_manager
        assert handler.auth_token == "secret"

    async def test_authenticate_no_auth_required(self) -> None:
        """Test authentication when no auth is required."""
        result = await self.handler._authenticate(self.websocket)

        assert result is True
        self.websocket.receive_json.assert_not_called()

    async def test_authenticate_success(self) -> None:
        """Test successful authentication."""
        # Setup handler with auth token
        handler = TelegramWebSocketHandler(session_manager=self.session_manager, auth_token="secret123")
        self.websocket.receive_json = AsyncMock(return_value={"type": "auth", "auth_token": "secret123"})

        # Execute
        result = await handler._authenticate(self.websocket)

        # Verify
        assert result is True
        self.websocket.send_json.assert_called_once()
        # Check that success message was sent
        call_args = self.websocket.send_json.call_args[0][0]
        assert call_args["type"] == EventType.AUTH_SUCCESS.value

    async def test_authenticate_wrong_token(self) -> None:
        """Test authentication with wrong token."""
        # Setup handler with auth token
        handler = TelegramWebSocketHandler(session_manager=self.session_manager, auth_token="secret123")
        self.websocket.receive_json = AsyncMock(return_value={"type": "auth", "auth_token": "wrong_token"})

        # Execute
        result = await handler._authenticate(self.websocket)

        # Verify
        assert result is False

    async def test_authenticate_wrong_message_type(self) -> None:
        """Test authentication with wrong message type."""
        # Setup handler with auth token
        handler = TelegramWebSocketHandler(session_manager=self.session_manager, auth_token="secret123")
        self.websocket.receive_json = AsyncMock(return_value={"type": "subscribe", "auth_token": "secret123"})

        # Execute
        result = await handler._authenticate(self.websocket)

        # Verify
        assert result is False

    async def test_authenticate_exception(self) -> None:
        """Test authentication with exception."""
        # Setup handler with auth token
        handler = TelegramWebSocketHandler(session_manager=self.session_manager, auth_token="secret123")
        self.websocket.receive_json = AsyncMock(side_effect=Exception("Connection error"))

        # Execute
        result = await handler._authenticate(self.websocket)

        # Verify
        assert result is False

    async def test_send_history(self) -> None:
        """Test sending message history."""
        # Execute
        await self.handler._send_history(self.websocket, self.session)

        # Verify
        self.websocket.send_json.assert_called_once()
        call_args = self.websocket.send_json.call_args[0][0]
        assert call_args["type"] == EventType.HISTORY.value
        assert len(call_args["messages"]) == 1

    async def test_send_history_error(self) -> None:
        """Test sending history with error."""
        # Setup
        self.websocket.send_json = AsyncMock(side_effect=Exception("Send error"))

        # Execute & Verify
        with pytest.raises(Exception, match="Send error"):
            await self.handler._send_history(self.websocket, self.session)

    async def test_handle_message_send_message_blocked(self) -> None:
        """Test handling send_message (bidirectional not supported yet)."""
        # Setup
        data = {"type": "send_message", "content": "Hello from web"}

        # Execute
        await self.handler._handle_message(self.websocket, "test-session-123", data)

        # Verify - should send error about bidirectional not supported
        self.websocket.send_json.assert_called_once()
        call_args = self.websocket.send_json.call_args[0][0]
        assert call_args["type"] == EventType.ERROR.value
        assert "not yet supported" in call_args["error"]

    async def test_handle_message_send_message_no_content(self) -> None:
        """Test handling send_message with no content."""
        # Setup
        data = {"type": "send_message", "content": ""}

        # Execute
        await self.handler._handle_message(self.websocket, "test-session-123", data)

        # Verify - should send error about missing content
        self.websocket.send_json.assert_called_once()
        call_args = self.websocket.send_json.call_args[0][0]
        assert call_args["type"] == EventType.ERROR.value
        assert "required" in call_args["error"]

    async def test_handle_message_ping(self) -> None:
        """Test handling ping message."""
        # Setup
        data = {"type": "ping"}

        # Execute
        await self.handler._handle_message(self.websocket, "test-session-123", data)

        # Verify - should send pong
        self.websocket.send_json.assert_called_once()
        call_args = self.websocket.send_json.call_args[0][0]
        assert call_args["type"] == "pong"

    async def test_handle_message_unknown_type(self) -> None:
        """Test handling unknown message type."""
        # Setup
        data = {"type": "unknown_type"}

        # Execute
        await self.handler._handle_message(self.websocket, "test-session-123", data)

        # Verify - should send error
        self.websocket.send_json.assert_called_once()
        call_args = self.websocket.send_json.call_args[0][0]
        assert call_args["type"] == EventType.ERROR.value
        assert "Unknown message type" in call_args["error"]


if __name__ == "__main__":
    unittest.main()
