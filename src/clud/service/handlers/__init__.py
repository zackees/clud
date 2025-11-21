"""HTTP request handlers for daemon service."""

from .agent_routes import (
    handle_get_agent,
    handle_heartbeat,
    handle_list_agents,
    handle_register_agent,
    handle_stop_agent,
)
from .daemon_routes import (
    handle_health,
    handle_telegram_start,
    handle_telegram_status,
    handle_telegram_stop,
)

__all__ = [
    "handle_register_agent",
    "handle_heartbeat",
    "handle_get_agent",
    "handle_list_agents",
    "handle_stop_agent",
    "handle_health",
    "handle_telegram_status",
    "handle_telegram_start",
    "handle_telegram_stop",
]
