"""Session manager for Telegram bot integration.

This module provides the SessionManager class which orchestrates Telegram conversations,
manages session state, and broadcasts events to connected web clients.
"""

import logging
import uuid
from datetime import datetime
from pathlib import Path

from fastapi import WebSocket

from clud.api.instance_manager import InstancePool
from clud.api.models import ClientType
from clud.telegram.models import (
    TelegramMessage,
    TelegramSession,
    WebSocketEvent,
)

logger = logging.getLogger(__name__)


class SessionManager:
    """Manages Telegram bot sessions and web client connections.

    The SessionManager is the core orchestration layer that:
    - Maintains session state per Telegram user
    - Routes messages between Telegram, clud instances, and web clients
    - Broadcasts events to connected web clients via WebSocket
    - Manages session lifecycle and cleanup
    """

    def __init__(
        self,
        instance_pool: InstancePool,
        max_sessions: int = 50,
        session_timeout_seconds: int = 3600,
        message_history_limit: int = 1000,
    ) -> None:
        """Initialize the session manager.

        Args:
            instance_pool: Pool for managing clud instances
            max_sessions: Maximum number of concurrent sessions
            session_timeout_seconds: Session timeout in seconds (default: 1 hour)
            message_history_limit: Maximum messages to keep in history
        """
        self.instance_pool = instance_pool
        self.max_sessions = max_sessions
        self.session_timeout_seconds = session_timeout_seconds
        self.message_history_limit = message_history_limit

        # Storage
        self.sessions: dict[str, TelegramSession] = {}
        self.user_to_session: dict[int, str] = {}
        self.web_clients: dict[str, set[WebSocket]] = {}  # session_id -> set of websockets

        logger.info(f"SessionManager initialized: max_sessions={max_sessions}, timeout={session_timeout_seconds}s, history_limit={message_history_limit}")

    async def get_or_create_session(
        self,
        telegram_user_id: int,
        telegram_username: str,
        telegram_first_name: str,
        telegram_last_name: str | None = None,
        working_directory: str | None = None,
    ) -> TelegramSession:
        """Get existing session for user or create new one.

        Args:
            telegram_user_id: Telegram user ID
            telegram_username: Telegram username
            telegram_first_name: User's first name
            telegram_last_name: User's last name (optional)
            working_directory: Working directory for clud instance

        Returns:
            TelegramSession (existing or newly created)

        Raises:
            RuntimeError: If max sessions limit reached
        """
        # Check if session exists
        if telegram_user_id in self.user_to_session:
            session_id = self.user_to_session[telegram_user_id]
            session = self.sessions[session_id]
            session.last_activity = datetime.now()
            logger.info(f"Reusing session {session_id} for user {telegram_user_id}")
            return session

        # Check max sessions limit
        if len(self.sessions) >= self.max_sessions:
            logger.error(f"Max sessions limit reached: {self.max_sessions}")
            raise RuntimeError(f"Maximum session limit reached ({self.max_sessions})")

        # Create new session
        session_id = str(uuid.uuid4())
        session = TelegramSession(
            session_id=session_id,
            telegram_user_id=telegram_user_id,
            telegram_username=telegram_username,
            telegram_first_name=telegram_first_name,
            telegram_last_name=telegram_last_name,
        )

        # Create clud instance for this session
        try:
            # Convert working_directory to Path if provided
            working_dir_path = Path(working_directory) if working_directory else None

            instance = await self.instance_pool.get_or_create_instance(
                session_id=session_id,
                client_type=ClientType.TELEGRAM,
                client_id=str(telegram_user_id),
                working_directory=working_dir_path,
            )
            session.instance_id = instance.instance_id
        except Exception as e:
            logger.error(f"Failed to create instance for session {session_id}: {e}")
            raise

        # Store session
        self.sessions[session_id] = session
        self.user_to_session[telegram_user_id] = session_id
        self.web_clients[session_id] = set()

        logger.info(f"Created new session {session_id} for user {telegram_user_id} (@{telegram_username})")
        return session

    def get_session(self, session_id: str) -> TelegramSession | None:
        """Get session by ID.

        Args:
            session_id: Session identifier

        Returns:
            TelegramSession or None if not found
        """
        return self.sessions.get(session_id)

    def get_user_session(self, telegram_user_id: int) -> TelegramSession | None:
        """Get session by Telegram user ID.

        Args:
            telegram_user_id: Telegram user ID

        Returns:
            TelegramSession or None if not found
        """
        session_id = self.user_to_session.get(telegram_user_id)
        if session_id:
            return self.sessions.get(session_id)
        return None

    def get_all_sessions(self) -> list[TelegramSession]:
        """Get all active sessions.

        Returns:
            List of all TelegramSession objects
        """
        return list(self.sessions.values())

    async def add_message(self, session_id: str, message: TelegramMessage) -> None:
        """Add a message to session history and broadcast to web clients.

        Args:
            session_id: Session identifier
            message: Message to add

        Raises:
            ValueError: If session not found
        """
        session = self.sessions.get(session_id)
        if not session:
            logger.error(f"Session {session_id} not found")
            raise ValueError(f"Session {session_id} not found")

        # Add message to history
        session.add_message(message)

        # Trim history if needed
        if len(session.message_history) > self.message_history_limit:
            session.message_history = session.message_history[-self.message_history_limit :]
            logger.debug(f"Trimmed message history for session {session_id} to {self.message_history_limit}")

        # Broadcast to web clients
        await self.broadcast_message(session_id, message)

        logger.debug(f"Added message {message.message_id} to session {session_id}")

    async def process_user_message(self, session_id: str, message_content: str, telegram_message_id: int) -> str:
        """Process a user message through the clud instance.

        Args:
            session_id: Session identifier
            message_content: User's message content
            telegram_message_id: Telegram's message ID

        Returns:
            Bot's response message

        Raises:
            ValueError: If session not found
            RuntimeError: If instance execution fails
        """
        session = self.sessions.get(session_id)
        if not session:
            logger.error(f"Session {session_id} not found")
            raise ValueError(f"Session {session_id} not found")

        # Create user message
        user_message = TelegramMessage.create_user_message(session_id=session_id, telegram_message_id=telegram_message_id, content=message_content)

        # Add to history and broadcast
        await self.add_message(session_id, user_message)

        # Send typing indicator
        await self.broadcast_typing(session_id, True)

        try:
            # Get instance
            instance = self.instance_pool.get_session_instance(session_id)
            if not instance:
                logger.error(f"No instance found for session {session_id}")
                raise RuntimeError("Instance not found")

            # Execute message
            result = await instance.execute(message_content)

            # Stop typing indicator
            await self.broadcast_typing(session_id, False)

            # Extract response
            response_content = result.get("output", "")
            if not response_content and result.get("error"):
                response_content = f"Error: {result['error']}"

            # Create bot message
            bot_message = TelegramMessage.create_bot_message(session_id=session_id, content=response_content, metadata={"exit_code": result.get("exit_code", 0)})

            # Add to history and broadcast
            await self.add_message(session_id, bot_message)

            return response_content

        except Exception as e:
            logger.error(f"Error processing message in session {session_id}: {e}")
            await self.broadcast_typing(session_id, False)

            # Send error message
            error_message = TelegramMessage.create_bot_message(session_id=session_id, content=f"Sorry, an error occurred: {str(e)}", metadata={"error": True})
            await self.add_message(session_id, error_message)

            raise RuntimeError(f"Failed to process message: {e}") from e

    async def register_web_client(self, session_id: str, websocket: WebSocket) -> None:
        """Register a web client for a session.

        Args:
            session_id: Session identifier
            websocket: WebSocket connection

        Raises:
            ValueError: If session not found
        """
        session = self.sessions.get(session_id)
        if not session:
            logger.error(f"Session {session_id} not found")
            raise ValueError(f"Session {session_id} not found")

        # Add to web clients set
        self.web_clients[session_id].add(websocket)
        session.web_client_count = len(self.web_clients[session_id])

        logger.info(f"Registered web client for session {session_id} ({session.web_client_count} total)")

        # Send connection confirmation and history
        await self._send_to_websocket(websocket, WebSocketEvent.connected(session_id))
        await self._send_to_websocket(websocket, WebSocketEvent.history(session.message_history))

    async def unregister_web_client(self, session_id: str, websocket: WebSocket) -> None:
        """Unregister a web client from a session.

        Args:
            session_id: Session identifier
            websocket: WebSocket connection
        """
        if session_id in self.web_clients:
            self.web_clients[session_id].discard(websocket)

            session = self.sessions.get(session_id)
            if session:
                session.web_client_count = len(self.web_clients[session_id])
                logger.info(f"Unregistered web client for session {session_id} ({session.web_client_count} remaining)")

    async def broadcast_message(self, session_id: str, message: TelegramMessage) -> None:
        """Broadcast a message to all connected web clients.

        Args:
            session_id: Session identifier
            message: Message to broadcast
        """
        event = WebSocketEvent.message(message)
        await self._broadcast_event(session_id, event)

    async def broadcast_typing(self, session_id: str, is_typing: bool) -> None:
        """Broadcast typing indicator to all connected web clients.

        Args:
            session_id: Session identifier
            is_typing: Whether the bot is typing
        """
        event = WebSocketEvent.typing(is_typing)
        await self._broadcast_event(session_id, event)

    async def broadcast_session_update(self, session_id: str) -> None:
        """Broadcast session update to all connected web clients.

        Args:
            session_id: Session identifier
        """
        session = self.sessions.get(session_id)
        if session:
            event = WebSocketEvent.session_update(session)
            await self._broadcast_event(session_id, event)

    async def _broadcast_event(self, session_id: str, event: WebSocketEvent) -> None:
        """Broadcast an event to all web clients for a session.

        Args:
            session_id: Session identifier
            event: Event to broadcast
        """
        if session_id not in self.web_clients:
            return

        disconnected: set[WebSocket] = set()
        for websocket in self.web_clients[session_id]:
            try:
                await self._send_to_websocket(websocket, event)
            except Exception as e:
                logger.warning(f"Failed to send to websocket: {e}")
                disconnected.add(websocket)

        # Clean up disconnected clients
        for websocket in disconnected:
            await self.unregister_web_client(session_id, websocket)

    async def _send_to_websocket(self, websocket: WebSocket, event: WebSocketEvent) -> None:
        """Send an event to a specific websocket.

        Args:
            websocket: WebSocket connection
            event: Event to send

        Raises:
            Exception: If send fails
        """
        await websocket.send_json(event.to_dict())

    async def cleanup_idle_sessions(self) -> int:
        """Clean up sessions that have been idle for too long.

        Returns:
            Number of sessions cleaned up
        """
        now = datetime.now()
        cleanup_count = 0

        # Find idle sessions
        sessions_to_delete: list[str] = []
        for session in self.sessions.values():
            idle_seconds = (now - session.last_activity).total_seconds()
            if idle_seconds > self.session_timeout_seconds:
                sessions_to_delete.append(session.session_id)
                logger.info(f"Session {session.session_id} idle for {idle_seconds:.0f}s, cleaning up")

        # Delete idle sessions
        for session_id in sessions_to_delete:
            await self.delete_session(session_id)
            cleanup_count += 1

        if cleanup_count > 0:
            logger.info(f"Cleaned up {cleanup_count} idle sessions")

        return cleanup_count

    async def delete_session(self, session_id: str) -> bool:
        """Delete a session and clean up resources.

        Args:
            session_id: Session identifier

        Returns:
            True if session was deleted, False if not found
        """
        session = self.sessions.get(session_id)
        if not session:
            logger.warning(f"Session {session_id} not found for deletion")
            return False

        # Close all web client connections
        if session_id in self.web_clients:
            for websocket in list(self.web_clients[session_id]):
                try:
                    await websocket.close(code=1000, reason="Session closed")
                except Exception as e:
                    logger.warning(f"Error closing websocket: {e}")
            del self.web_clients[session_id]

        # Delete instance
        if session.instance_id:
            await self.instance_pool.delete_instance(session.instance_id)

        # Remove from storage
        del self.sessions[session_id]
        if session.telegram_user_id in self.user_to_session:
            del self.user_to_session[session.telegram_user_id]

        logger.info(f"Deleted session {session_id}")
        return True

    async def shutdown(self) -> None:
        """Shut down all sessions and cleanup resources."""
        logger.info(f"Shutting down SessionManager with {len(self.sessions)} sessions")

        # Close all sessions
        for session_id in list(self.sessions.keys()):
            await self.delete_session(session_id)

        logger.info("SessionManager shutdown complete")
