"""WebSocket server for Telegram integration.

Provides real-time bidirectional communication between the web client and Telegram sessions.
"""

import json
import logging
from typing import Any

from fastapi import WebSocket, WebSocketDisconnect, status

from clud.telegram.models import EventType, WebSocketEvent
from clud.telegram.session_manager import SessionManager

logger = logging.getLogger(__name__)


class TelegramWebSocketHandler:
    """Handles WebSocket connections for Telegram sessions."""

    def __init__(self, session_manager: SessionManager, auth_token: str | None = None) -> None:
        """Initialize the WebSocket handler.

        Args:
            session_manager: The session manager instance
            auth_token: Optional auth token for web client authentication
        """
        self.session_manager = session_manager
        self.auth_token = auth_token

    async def handle_connection(self, websocket: WebSocket, session_id: str) -> None:
        """Handle a WebSocket connection for a specific session.

        Args:
            websocket: The WebSocket connection
            session_id: The session ID to connect to
        """
        await websocket.accept()
        logger.info(f"WebSocket connection accepted for session {session_id}")

        try:
            # Authenticate if auth token is required
            if not await self._authenticate(websocket):
                logger.warning(f"Authentication failed for session {session_id}")
                await websocket.close(code=status.WS_1008_POLICY_VIOLATION, reason="Unauthorized")
                return

            # Verify session exists
            session = self.session_manager.get_session(session_id)
            if not session:
                logger.warning(f"Session {session_id} not found")
                await websocket.close(code=status.WS_1008_POLICY_VIOLATION, reason="Session not found")
                return

            # Register web client with session manager
            await self.session_manager.register_web_client(session_id, websocket)
            logger.info(f"Web client registered for session {session_id}")

            # Send message history (replay)
            await self._send_history(websocket, session)

            # Listen for incoming messages
            await self._message_loop(websocket, session_id)

        except WebSocketDisconnect:
            logger.info(f"WebSocket disconnected for session {session_id}")
        except Exception as e:
            logger.error(f"Error in WebSocket handler for session {session_id}: {e}", exc_info=True)
        finally:
            # Unregister web client
            await self.session_manager.unregister_web_client(session_id, websocket)
            logger.info(f"Web client unregistered for session {session_id}")

    async def _authenticate(self, websocket: WebSocket) -> bool:
        """Authenticate the web client.

        Args:
            websocket: The WebSocket connection

        Returns:
            True if authentication successful or not required, False otherwise
        """
        if not self.auth_token:
            # No auth required
            return True

        try:
            # Wait for auth message
            data = await websocket.receive_json()

            if data.get("type") != "auth":
                logger.warning("Expected auth message, got: %s", data.get("type"))
                return False

            provided_token = data.get("auth_token")
            if provided_token == self.auth_token:
                # Send success response
                event = WebSocketEvent(event_type=EventType.AUTH_SUCCESS, data={"message": "Authentication successful"})
                await websocket.send_json(event.to_dict())
                return True

            logger.warning("Invalid auth token provided")
            return False

        except Exception as e:
            logger.error(f"Error during authentication: {e}", exc_info=True)
            return False

    async def _send_history(self, websocket: WebSocket, session: Any) -> None:
        """Send message history to the web client.

        Args:
            websocket: The WebSocket connection
            session: The session object
        """
        try:
            history_event = WebSocketEvent.history(session.message_history)
            await websocket.send_json(history_event.to_dict())
            logger.info(f"Sent {len(session.message_history)} messages to web client")
        except Exception as e:
            logger.error(f"Error sending history: {e}", exc_info=True)
            raise

    async def _message_loop(self, websocket: WebSocket, session_id: str) -> None:
        """Main message loop for receiving messages from web client.

        Args:
            websocket: The WebSocket connection
            session_id: The session ID
        """
        while True:
            try:
                data = await websocket.receive_json()
                await self._handle_message(websocket, session_id, data)
            except WebSocketDisconnect:
                logger.info(f"Client disconnected from session {session_id}")
                raise
            except json.JSONDecodeError as e:
                logger.warning(f"Invalid JSON received: {e}")
                error_event = WebSocketEvent.error("Invalid JSON format")
                await websocket.send_json(error_event.to_dict())
            except Exception as e:
                logger.error(f"Error in message loop: {e}", exc_info=True)
                error_event = WebSocketEvent.error(f"Server error: {str(e)}")
                await websocket.send_json(error_event.to_dict())

    async def _handle_message(self, websocket: WebSocket, session_id: str, data: dict[str, Any]) -> None:
        """Handle a message from the web client.

        Args:
            websocket: The WebSocket connection
            session_id: The session ID
            data: The message data
        """
        message_type = data.get("type")

        if message_type == "send_message":
            # Handle web-initiated message
            content = data.get("content")
            if not content:
                error_event = WebSocketEvent.error("Message content is required")
                await websocket.send_json(error_event.to_dict())
                return

            # Check if bidirectional messaging is enabled
            # For now, we'll assume it's disabled and return an error
            # This will be implemented in Phase 4
            error_event = WebSocketEvent.error("Bidirectional messaging is not yet supported")
            await websocket.send_json(error_event.to_dict())
            logger.info(f"Bidirectional message blocked for session {session_id}")

        elif message_type == "ping":
            # Respond to ping with pong
            pong_event = WebSocketEvent(
                event_type=EventType.SESSION_UPDATE,
                data={"type": "pong"},
            )
            await websocket.send_json(pong_event.to_dict())

        else:
            logger.warning(f"Unknown message type: {message_type}")
            error_event = WebSocketEvent.error(f"Unknown message type: {message_type}")
            await websocket.send_json(error_event.to_dict())


async def telegram_websocket_endpoint(websocket: WebSocket, session_id: str, session_manager: SessionManager, auth_token: str | None = None) -> None:
    """FastAPI WebSocket endpoint for Telegram sessions.

    Args:
        websocket: The WebSocket connection
        session_id: The session ID to connect to
        session_manager: The session manager instance
        auth_token: Optional auth token for authentication
    """
    handler = TelegramWebSocketHandler(session_manager, auth_token)
    await handler.handle_connection(websocket, session_id)
