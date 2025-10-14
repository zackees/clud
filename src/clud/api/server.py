"""FastAPI server for message handling API."""

import json
import logging
from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager
from typing import Any

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse

from clud.api.message_handler import MessageHandler
from clud.api.models import MessageRequest

logger = logging.getLogger(__name__)

# Global message handler instance
_message_handler: MessageHandler | None = None


def get_message_handler() -> MessageHandler:
    """Get the global message handler instance."""
    global _message_handler
    if _message_handler is None:
        _message_handler = MessageHandler()
    return _message_handler


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncGenerator[None, None]:
    """Lifespan context manager for startup and shutdown events."""
    # Startup
    logger.info("Starting API server...")
    handler = get_message_handler()
    await handler.start_cleanup_task()
    logger.info("API server started")

    yield

    # Shutdown
    logger.info("Shutting down API server...")
    await handler.shutdown()
    logger.info("API server shutdown complete")


def create_app() -> FastAPI:
    """
    Create and configure the FastAPI application.

    Returns:
        Configured FastAPI app
    """
    app = FastAPI(
        title="Clud Message Handler API",
        description="API for routing messages to clud instances",
        version="0.1.0",
        lifespan=lifespan,
    )

    # Configure CORS
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],  # In production, should be more restrictive
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    @app.get("/health")
    async def health_check() -> dict[str, str]:
        """Health check endpoint."""
        return {"status": "ok"}

    @app.post("/api/message")
    async def handle_message(data: dict[str, Any]) -> dict[str, Any]:
        """
        Handle an incoming message from a client.

        Args:
            data: Message request data

        Returns:
            Message response
        """
        try:
            # Parse request
            request = MessageRequest.from_dict(data)

            # Handle message
            handler = get_message_handler()
            response = await handler.handle_message(request)

            return response.to_dict()

        except KeyError as e:
            logger.warning(f"Missing required field in request: {e}")
            raise HTTPException(status_code=400, detail=f"Missing required field: {e}") from e
        except ValueError as e:
            logger.warning(f"Invalid value in request: {e}")
            raise HTTPException(status_code=400, detail=f"Invalid value: {e}") from e
        except Exception as e:
            logger.exception(f"Error handling message: {e}")
            raise HTTPException(status_code=500, detail="Internal server error") from e

    @app.get("/api/instances")
    async def list_instances() -> dict[str, Any]:
        """
        List all active instances.

        Returns:
            List of instance information
        """
        try:
            handler = get_message_handler()
            instances = handler.get_all_instances()

            return {
                "instances": [instance.to_dict() for instance in instances],
                "count": len(instances),
            }

        except Exception as e:
            logger.exception(f"Error listing instances: {e}")
            raise HTTPException(status_code=500, detail="Internal server error") from e

    @app.get("/api/instances/{instance_id}")
    async def get_instance(instance_id: str) -> dict[str, Any]:
        """
        Get information about a specific instance.

        Args:
            instance_id: The instance ID

        Returns:
            Instance information
        """
        try:
            handler = get_message_handler()
            instance = handler.get_instance(instance_id)

            if instance is None:
                raise HTTPException(status_code=404, detail="Instance not found")

            return instance.to_dict()

        except HTTPException:
            raise
        except Exception as e:
            logger.exception(f"Error getting instance: {e}")
            raise HTTPException(status_code=500, detail="Internal server error") from e

    @app.delete("/api/instances/{instance_id}")
    async def delete_instance(instance_id: str) -> dict[str, Any]:
        """
        Delete an instance.

        Args:
            instance_id: The instance ID

        Returns:
            Success message
        """
        try:
            handler = get_message_handler()
            deleted = await handler.delete_instance(instance_id)

            if not deleted:
                raise HTTPException(status_code=404, detail="Instance not found")

            return {"status": "deleted", "instance_id": instance_id}

        except HTTPException:
            raise
        except Exception as e:
            logger.exception(f"Error deleting instance: {e}")
            raise HTTPException(status_code=500, detail="Internal server error") from e

    @app.post("/api/cleanup")
    async def cleanup_idle_instances(max_idle_seconds: int = 1800) -> dict[str, Any]:
        """
        Clean up idle instances.

        Args:
            max_idle_seconds: Maximum idle time in seconds (default: 30 minutes)

        Returns:
            Number of instances cleaned up
        """
        try:
            handler = get_message_handler()
            count = await handler.cleanup_idle_instances(max_idle_seconds)

            return {"status": "ok", "cleaned_up": count}

        except Exception as e:
            logger.exception(f"Error cleaning up instances: {e}")
            raise HTTPException(status_code=500, detail="Internal server error") from e

    @app.websocket("/ws/{session_id}")
    async def websocket_endpoint(websocket: WebSocket, session_id: str) -> None:
        """
        WebSocket endpoint for real-time output streaming.

        Args:
            websocket: WebSocket connection
            session_id: Session identifier

        Note:
            This is a basic WebSocket implementation. In the future, this will
            stream real-time output from clud instances as they execute.
            For now, it provides connection management infrastructure.
        """
        await websocket.accept()
        logger.info(f"WebSocket connection established for session {session_id}")

        try:
            # Send connection confirmation
            await websocket.send_json({"type": "connected", "session_id": session_id})

            # Keep connection alive and handle incoming messages
            while True:
                try:
                    # Wait for messages from client
                    data = await websocket.receive_text()
                    message_data = json.loads(data)

                    # Handle message based on type
                    if message_data.get("type") == "ping":
                        await websocket.send_json({"type": "pong"})
                    elif message_data.get("type") == "execute":
                        # Execute message and stream output
                        request = MessageRequest.from_dict(
                            {
                                "message": message_data.get("message", ""),
                                "session_id": session_id,
                                "client_type": "web",
                                "client_id": message_data.get("client_id", "websocket"),
                            }
                        )

                        handler = get_message_handler()
                        response = await handler.handle_message(request)

                        # Send response
                        await websocket.send_json(
                            {
                                "type": "response",
                                "data": response.to_dict(),
                            }
                        )
                    else:
                        await websocket.send_json(
                            {
                                "type": "error",
                                "message": f"Unknown message type: {message_data.get('type')}",
                            }
                        )

                except WebSocketDisconnect:
                    logger.info(f"WebSocket disconnected for session {session_id}")
                    break
                except json.JSONDecodeError as e:
                    logger.warning(f"Invalid JSON received on WebSocket: {e}")
                    await websocket.send_json({"type": "error", "message": "Invalid JSON"})
                except Exception as e:
                    logger.exception(f"Error in WebSocket handler: {e}")
                    await websocket.send_json({"type": "error", "message": str(e)})

        except Exception as e:
            logger.exception(f"WebSocket error for session {session_id}: {e}")
        finally:
            logger.info(f"WebSocket connection closed for session {session_id}")

    @app.exception_handler(Exception)
    async def global_exception_handler(request: Any, exc: Exception) -> JSONResponse:
        """Global exception handler."""
        logger.exception(f"Unhandled exception: {exc}")
        return JSONResponse(
            status_code=500,
            content={"detail": "Internal server error"},
        )

    return app
