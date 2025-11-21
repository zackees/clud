"""WebSocket route handlers for Claude Code Web UI."""

import contextlib
import logging
import os

from fastapi import FastAPI, WebSocket, WebSocketDisconnect

from .api import ChatHandler
from .telegram_api import TelegramAPIHandler
from .terminal_handler import TerminalHandler

logger = logging.getLogger(__name__)


def register_websocket_routes(
    app: FastAPI,
    chat_handler: ChatHandler,
    terminal_handler: TerminalHandler,
    telegram_handler: TelegramAPIHandler,
) -> None:
    """Register WebSocket routes with the FastAPI application.

    Args:
        app: FastAPI application instance
        chat_handler: Handler for chat WebSocket connections
        terminal_handler: Handler for terminal WebSocket connections
        telegram_handler: Handler for Telegram API operations
    """

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
                    project_path = data.get("project_path")

                    # Validate project_path - if empty, invalid, or just "/", use server's cwd
                    if not project_path or project_path == "/" or not os.path.isdir(project_path):
                        project_path = os.getcwd()

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

    @app.websocket("/ws/term")
    async def terminal_websocket(websocket: WebSocket, id: str) -> None:
        """WebSocket endpoint for terminal sessions.

        Args:
            websocket: WebSocket connection
            id: Terminal session identifier
        """
        await terminal_handler.handle_websocket(websocket, id)

    @app.websocket("/ws/telegram")
    async def telegram_websocket(websocket: WebSocket) -> None:
        """WebSocket endpoint for Telegram real-time updates.

        This endpoint provides real-time updates for Telegram bot messages and status.
        Currently a stub endpoint - full Telegram integration is work in progress.
        """
        await websocket.accept()
        logger.info("Telegram WebSocket client connected: %s", websocket.client)

        try:
            # Send initial connection acknowledgment
            await websocket.send_json({"type": "telegram_status", "status": "connected"})

            while True:
                # Receive and handle messages from client
                data = await websocket.receive_json()
                message_type = data.get("type")

                if message_type == "ping":
                    await websocket.send_json({"type": "pong"})
                elif message_type == "telegram_send":
                    # Handle outgoing message request
                    chat_id = data.get("chat_id")
                    message = data.get("message")
                    logger.info("Telegram send request: chat_id=%s", chat_id)

                    # Use telegram handler to send message
                    success = await telegram_handler.send_message(chat_id, message)

                    if success:
                        await websocket.send_json({"type": "telegram_sent", "status": "ok"})
                    else:
                        await websocket.send_json({"type": "telegram_error", "error": "Failed to send message"})
                else:
                    logger.warning("Unknown Telegram WebSocket message type: %s", message_type)
                    await websocket.send_json({"type": "error", "error": f"Unknown message type: {message_type}"})

        except WebSocketDisconnect:
            logger.info("Telegram WebSocket client disconnected: %s", websocket.client)
        except Exception as e:
            logger.exception("Error handling Telegram WebSocket connection")
            with contextlib.suppress(Exception):
                await websocket.send_json({"type": "error", "error": str(e)})
