"""REST API endpoints for Telegram integration.

Provides HTTP endpoints for session management and monitoring.
"""

import logging
import secrets
from typing import Any

from fastapi import APIRouter, HTTPException, status
from pydantic import BaseModel, Field

from clud.telegram.models import ContentType
from clud.telegram.session_manager import SessionManager

logger = logging.getLogger(__name__)


# Request/Response Models


class SessionListResponse(BaseModel):
    """Response model for session list."""

    sessions: list[dict[str, Any]]
    total: int


class SessionDetailResponse(BaseModel):
    """Response model for session detail."""

    session: dict[str, Any]
    message_count: int


class SendMessageRequest(BaseModel):
    """Request model for sending a message."""

    content: str = Field(..., min_length=1, max_length=4096)
    content_type: ContentType = ContentType.TEXT


class SendMessageResponse(BaseModel):
    """Response model for sending a message."""

    message_id: str
    success: bool


class DeleteSessionResponse(BaseModel):
    """Response model for deleting a session."""

    success: bool
    message: str


class AuthTokenResponse(BaseModel):
    """Response model for auth token."""

    token: str
    expires_in: int | None = None


# API Router


def create_telegram_api_router(session_manager: SessionManager, auth_token: str | None = None) -> APIRouter:
    """Create the FastAPI router for Telegram API endpoints.

    Args:
        session_manager: The session manager instance
        auth_token: Optional auth token for protected endpoints

    Returns:
        The configured APIRouter
    """
    router = APIRouter(prefix="/api/telegram", tags=["telegram"])

    @router.get("/sessions", response_model=SessionListResponse)
    async def list_sessions() -> SessionListResponse:
        """List all active Telegram sessions.

        Returns:
            List of sessions with basic info
        """
        try:
            sessions = session_manager.get_all_sessions()
            session_dicts: list[dict[str, Any]] = []

            for session in sessions:
                session_dict: dict[str, Any] = {
                    "session_id": session.session_id,
                    "telegram_user_id": session.telegram_user_id,
                    "telegram_username": session.telegram_username,
                    "display_name": session.get_display_name(),
                    "message_count": len(session.message_history),
                    "created_at": session.created_at.isoformat(),
                    "last_activity": session.last_activity.isoformat(),
                    "is_active": session.is_active,
                    "web_client_count": session.web_client_count,
                }

                # Add last message preview if available
                last_msg = session.get_last_message()
                if last_msg:
                    session_dict["last_message"] = {
                        "sender": last_msg.sender.value,
                        "content_preview": last_msg.content[:100] if len(last_msg.content) > 100 else last_msg.content,
                        "timestamp": last_msg.timestamp.isoformat(),
                    }

                session_dicts.append(session_dict)

            # Sort by last activity (most recent first)
            session_dicts.sort(key=lambda x: x["last_activity"], reverse=True)

            return SessionListResponse(sessions=session_dicts, total=len(session_dicts))

        except Exception as e:
            logger.error(f"Error listing sessions: {e}", exc_info=True)
            raise HTTPException(status_code=status.HTTP_500_INTERNAL_SERVER_ERROR, detail=f"Failed to list sessions: {str(e)}") from e

    @router.get("/sessions/{session_id}", response_model=SessionDetailResponse)
    async def get_session(session_id: str) -> SessionDetailResponse:
        """Get detailed information about a specific session.

        Args:
            session_id: The session ID

        Returns:
            Detailed session information including full message history
        """
        try:
            session = session_manager.get_session(session_id)
            if not session:
                raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found")

            session_dict: dict[str, Any] = {
                "session_id": session.session_id,
                "telegram_user_id": session.telegram_user_id,
                "telegram_username": session.telegram_username,
                "telegram_first_name": session.telegram_first_name,
                "telegram_last_name": session.telegram_last_name,
                "display_name": session.get_display_name(),
                "instance_id": session.instance_id,
                "created_at": session.created_at.isoformat(),
                "last_activity": session.last_activity.isoformat(),
                "is_active": session.is_active,
                "web_client_count": session.web_client_count,
                "message_history": [msg.to_dict() for msg in session.message_history],
            }

            return SessionDetailResponse(session=session_dict, message_count=len(session.message_history))

        except HTTPException:
            raise
        except Exception as e:
            logger.error(f"Error getting session {session_id}: {e}", exc_info=True)
            raise HTTPException(status_code=status.HTTP_500_INTERNAL_SERVER_ERROR, detail=f"Failed to get session: {str(e)}") from e

    @router.post("/sessions/{session_id}/message", response_model=SendMessageResponse)
    async def send_message(session_id: str, request: SendMessageRequest) -> SendMessageResponse:
        """Send a message from the web client to the Telegram bot.

        This endpoint enables bidirectional messaging (web → Telegram).
        Currently not implemented (Phase 4 feature).

        Args:
            session_id: The session ID
            request: The message to send

        Returns:
            Response with message ID and success status
        """
        # This feature will be implemented in Phase 4
        raise HTTPException(
            status_code=status.HTTP_501_NOT_IMPLEMENTED,
            detail="Bidirectional messaging is not yet supported. This feature will be available in Phase 4.",
        )

    @router.delete("/sessions/{session_id}", response_model=DeleteSessionResponse)
    async def delete_session(session_id: str) -> DeleteSessionResponse:
        """Delete a Telegram session and clean up resources.

        Args:
            session_id: The session ID to delete

        Returns:
            Success status and message
        """
        try:
            session = session_manager.get_session(session_id)
            if not session:
                raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found")

            # Delete the session
            await session_manager.delete_session(session_id)

            logger.info(f"Session {session_id} deleted via API")
            return DeleteSessionResponse(success=True, message=f"Session {session_id} deleted successfully")

        except HTTPException:
            raise
        except Exception as e:
            logger.error(f"Error deleting session {session_id}: {e}", exc_info=True)
            raise HTTPException(status_code=status.HTTP_500_INTERNAL_SERVER_ERROR, detail=f"Failed to delete session: {str(e)}") from e

    @router.get("/auth", response_model=AuthTokenResponse)
    async def get_auth_token() -> AuthTokenResponse:
        """Get an authentication token for the web client.

        This endpoint returns the configured auth token or generates a temporary one.

        Returns:
            Auth token and expiration info
        """
        if auth_token:
            # Return the configured auth token
            return AuthTokenResponse(token=auth_token, expires_in=None)
        else:
            # Generate a temporary session token (valid for 1 hour)
            temp_token = secrets.token_urlsafe(32)
            logger.info("Generated temporary auth token")
            return AuthTokenResponse(token=temp_token, expires_in=3600)

    @router.get("/health")
    async def health_check() -> dict[str, Any]:
        """Health check endpoint.

        Returns:
            Health status and statistics
        """
        sessions = session_manager.get_all_sessions()
        active_sessions = [s for s in sessions if s.is_active]
        total_messages = sum(len(s.message_history) for s in sessions)

        return {
            "status": "healthy",
            "total_sessions": len(sessions),
            "active_sessions": len(active_sessions),
            "total_messages": total_messages,
        }

    return router
