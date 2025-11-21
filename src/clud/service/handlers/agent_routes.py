"""Agent registration and management HTTP request handlers."""

import http.server
import json
import logging
from typing import Any

from ..models import AgentInfo, AgentStatus
from ..registry import AgentRegistry

logger = logging.getLogger(__name__)


def _send_json_response(handler: http.server.BaseHTTPRequestHandler, data: dict[str, Any], status: int = 200) -> None:
    """Send JSON response.

    Args:
        handler: HTTP request handler instance
        data: Dictionary to send as JSON
        status: HTTP status code
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
        status: HTTP status code
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


def handle_register_agent(handler: http.server.BaseHTTPRequestHandler, registry: AgentRegistry) -> None:
    """Handle agent registration.

    Args:
        handler: HTTP request handler instance
        registry: Agent registry for storing agent information
    """
    logger.debug("Received agent registration request")

    data = _read_json_body(handler)
    if not data:
        logger.warning("Registration request missing body")
        _send_error_response(handler, "Missing request body")
        return

    logger.debug(f"Registration data: {data}")

    required_fields = ["agent_id", "cwd", "pid", "command"]
    if not all(field in data for field in required_fields):
        logger.warning(f"Registration missing required fields: {[f for f in required_fields if f not in data]}")
        _send_error_response(handler, f"Missing required fields: {required_fields}")
        return

    try:
        agent = AgentInfo(
            agent_id=data["agent_id"],
            cwd=data["cwd"],
            pid=data["pid"],
            command=data["command"],
            status=AgentStatus.STARTING,
            capabilities=data.get("capabilities", {}),
        )
        logger.debug(f"Created AgentInfo: agent_id={agent.agent_id}, pid={agent.pid}")
        registry.register(agent)
        logger.info(f"Registered agent: {agent.agent_id}")
        _send_json_response(handler, {"status": "registered", "agent_id": agent.agent_id}, 201)
    except Exception as e:
        logger.error(f"Error registering agent: {e}", exc_info=True)
        _send_error_response(handler, f"Registration failed: {e}", 500)


def handle_heartbeat(handler: http.server.BaseHTTPRequestHandler, registry: AgentRegistry, agent_id: str) -> None:
    """Handle agent heartbeat.

    Args:
        handler: HTTP request handler instance
        registry: Agent registry for updating heartbeat
        agent_id: ID of the agent sending heartbeat
    """
    logger.debug(f"Received heartbeat for agent: {agent_id}")

    data = _read_json_body(handler) or {}
    logger.debug(f"Heartbeat data: {data}")

    # Extract optional status update
    status = None
    if "status" in data:
        try:
            status = AgentStatus(data["status"])
            logger.debug(f"Heartbeat includes status update: {status.value}")
        except ValueError:
            logger.warning(f"Invalid status in heartbeat: {data['status']}")
            _send_error_response(handler, f"Invalid status: {data['status']}")
            return
        # Remove status from data to avoid overwriting with raw string
        data = {k: v for k, v in data.items() if k != "status"}

    # Update heartbeat
    success = registry.update_heartbeat(agent_id, status=status, **data)

    if success:
        logger.debug(f"Heartbeat updated successfully for agent: {agent_id}")
        _send_json_response(handler, {"status": "ok"})
    else:
        logger.warning(f"Heartbeat failed - agent not found: {agent_id}")
        _send_error_response(handler, "Agent not found", 404)


def handle_get_agent(handler: http.server.BaseHTTPRequestHandler, registry: AgentRegistry, agent_id: str) -> None:
    """Handle get agent by ID.

    Args:
        handler: HTTP request handler instance
        registry: Agent registry for retrieving agent information
        agent_id: ID of the agent to retrieve
    """
    agent = registry.get(agent_id)
    if agent:
        _send_json_response(handler, agent.to_dict())
    else:
        _send_error_response(handler, "Agent not found", 404)


def handle_list_agents(handler: http.server.BaseHTTPRequestHandler, registry: AgentRegistry) -> None:
    """Handle list all agents.

    Args:
        handler: HTTP request handler instance
        registry: Agent registry for retrieving agent list
    """
    agents = registry.list_all()
    _send_json_response(handler, {"agents": [agent.to_dict() for agent in agents]})


def handle_stop_agent(handler: http.server.BaseHTTPRequestHandler, registry: AgentRegistry, agent_id: str) -> None:
    """Handle stop agent request.

    Args:
        handler: HTTP request handler instance
        registry: Agent registry for marking agent as stopped
        agent_id: ID of the agent to stop
    """
    logger.debug(f"Received stop request for agent: {agent_id}")

    data = _read_json_body(handler) or {}
    exit_code = data.get("exit_code", 0)
    logger.debug(f"Stop request data: exit_code={exit_code}")

    success = registry.mark_stopped(agent_id, exit_code)
    if success:
        logger.info(f"Agent stopped successfully: {agent_id} (exit_code={exit_code})")
        _send_json_response(handler, {"status": "stopped"})
    else:
        logger.warning(f"Stop failed - agent not found: {agent_id}")
        _send_error_response(handler, "Agent not found", 404)
