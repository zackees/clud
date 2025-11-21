"""Daemon-related HTTP request handlers."""

import http.server
import json
import logging
import os
from typing import Any

from ..models import AgentStatus
from ..registry import AgentRegistry

logger = logging.getLogger(__name__)


def _send_json_response(handler: http.server.BaseHTTPRequestHandler, data: dict[str, Any], status: int = 200) -> None:
    """Send JSON response.

    Args:
        handler: HTTP request handler instance
        data: Response data dictionary
        status: HTTP status code (default: 200)
    """
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.end_headers()
    handler.wfile.write(json.dumps(data).encode("utf-8"))


def _send_error_response(handler: http.server.BaseHTTPRequestHandler, message: str, status: int = 400) -> None:
    """Send error response.

    Args:
        handler: HTTP request handler instance
        message: Error message
        status: HTTP status code (default: 400)
    """
    _send_json_response(handler, {"error": message}, status)


def _read_json_body(handler: http.server.BaseHTTPRequestHandler) -> dict[str, Any] | None:
    """Read and parse JSON body.

    Args:
        handler: HTTP request handler instance

    Returns:
        Parsed JSON data or None if no body or invalid JSON
    """
    content_length = int(handler.headers.get("Content-Length", 0))
    if content_length == 0:
        return None

    body = handler.rfile.read(content_length)
    try:
        return json.loads(body.decode("utf-8"))
    except json.JSONDecodeError as e:
        logger.warning(f"Invalid JSON in request: {e}")
        return None


def handle_health(handler: http.server.BaseHTTPRequestHandler, registry: AgentRegistry) -> None:
    """Handle health check.

    Args:
        handler: HTTP request handler instance
        registry: Agent registry instance
    """
    agent_count = len(registry.list_all())
    running_count = len(registry.list_by_status(AgentStatus.RUNNING))
    stale_count = len(registry.list_stale())

    _send_json_response(
        handler,
        {
            "status": "ok",
            "pid": os.getpid(),
            "agents": {"total": agent_count, "running": running_count, "stale": stale_count},
        },
    )


def handle_telegram_status(handler: http.server.BaseHTTPRequestHandler, telegram_manager: Any) -> None:
    """Handle telegram service status request.

    Args:
        handler: HTTP request handler instance
        telegram_manager: TelegramServiceManager instance
    """
    status = telegram_manager.get_status()
    _send_json_response(handler, status)


def handle_telegram_start(handler: http.server.BaseHTTPRequestHandler, telegram_manager: Any) -> None:
    """Handle telegram service start request.

    Args:
        handler: HTTP request handler instance
        telegram_manager: TelegramServiceManager instance
    """
    logger.debug("Received telegram start request")

    data = _read_json_body(handler) or {}
    config_path = data.get("config_path")
    port = data.get("port")

    try:
        success = telegram_manager.start_service(config_path=config_path, port=port)
        if success:
            _send_json_response(handler, {"status": "started"}, 201)
        else:
            _send_error_response(handler, "Failed to start telegram service", 500)
    except Exception as e:
        logger.error(f"Error starting telegram service: {e}")
        _send_error_response(handler, f"Failed to start telegram service: {e}", 500)


def handle_telegram_stop(handler: http.server.BaseHTTPRequestHandler, telegram_manager: Any) -> None:
    """Handle telegram service stop request.

    Args:
        handler: HTTP request handler instance
        telegram_manager: TelegramServiceManager instance
    """
    logger.debug("Received telegram stop request")

    try:
        success = telegram_manager.stop_service()
        if success:
            _send_json_response(handler, {"status": "stopped"})
        else:
            _send_error_response(handler, "Telegram service not running", 400)
    except Exception as e:
        logger.error(f"Error stopping telegram service: {e}")
        _send_error_response(handler, f"Failed to stop telegram service: {e}", 500)
