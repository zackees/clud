"""Hook system for intercepting and forwarding execution events.

This module provides a flexible hook infrastructure that allows external
systems (Telegram, webhooks, etc.) to receive real-time updates about
clud agent execution.
"""

import asyncio
import logging
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any, Protocol

logger = logging.getLogger(__name__)


class HookEvent(Enum):
    """Types of events that can trigger hooks."""

    PRE_EXECUTION = "pre_execution"  # Before agent starts executing
    POST_EXECUTION = "post_execution"  # After agent completes
    OUTPUT_CHUNK = "output_chunk"  # Streaming output chunk
    ERROR = "error"  # Error occurred during execution
    AGENT_START = "agent_start"  # Agent process started
    AGENT_STOP = "agent_stop"  # Agent process stopped


@dataclass
class HookContext:
    """Context information passed to hook handlers.

    Attributes:
        event: The type of event that triggered this hook
        instance_id: Unique identifier for this agent instance
        session_id: Session identifier (e.g., Telegram chat_id)
        client_type: Type of client (telegram, webhook, api)
        client_id: Identifier for the specific client
        timestamp: When this event occurred
        message: Optional message content
        output: Optional output content
        error: Optional error information
        metadata: Additional event-specific data
    """

    event: HookEvent
    instance_id: str
    session_id: str
    client_type: str
    client_id: str
    timestamp: datetime = field(default_factory=datetime.now)
    message: str | None = None
    output: str | None = None
    error: str | None = None
    metadata: dict[str, Any] = field(default_factory=lambda: {})


class HookHandler(Protocol):
    """Protocol for hook handler implementations.

    Hook handlers must implement the handle() method to process events.
    """

    async def handle(self, context: HookContext) -> None:
        """Handle a hook event.

        Args:
            context: The hook context containing event information
        """
        ...


class HookManager:
    """Manages hook registration and event triggering.

    The HookManager maintains a registry of hook handlers and provides
    methods to register handlers and trigger events that will be
    distributed to all registered handlers.
    """

    def __init__(self) -> None:
        """Initialize the hook manager."""
        self._handlers: dict[HookEvent, list[HookHandler]] = {}
        self._global_handlers: list[HookHandler] = []

    def register(self, handler: HookHandler, events: list[HookEvent] | None = None) -> None:
        """Register a hook handler for specific events or all events.

        Args:
            handler: The hook handler to register
            events: List of events to handle, or None for all events
        """
        if events is None:
            self._global_handlers.append(handler)
            logger.debug(f"Registered global hook handler: {handler.__class__.__name__}")
        else:
            for event in events:
                if event not in self._handlers:
                    self._handlers[event] = []
                self._handlers[event].append(handler)
                logger.debug(f"Registered hook handler {handler.__class__.__name__} for event {event.value}")

    def unregister(self, handler: HookHandler) -> None:
        """Unregister a hook handler from all events.

        Args:
            handler: The hook handler to unregister
        """
        # Remove from global handlers
        if handler in self._global_handlers:
            self._global_handlers.remove(handler)

        # Remove from event-specific handlers
        for event_handlers in self._handlers.values():
            if handler in event_handlers:
                event_handlers.remove(handler)

        logger.debug(f"Unregistered hook handler: {handler.__class__.__name__}")

    async def trigger(self, context: HookContext) -> None:
        """Trigger a hook event and notify all registered handlers.

        Args:
            context: The hook context containing event information
        """
        # Collect all handlers that should be notified
        handlers_to_notify: list[HookHandler] = []

        # Add event-specific handlers
        if context.event in self._handlers:
            handlers_to_notify.extend(self._handlers[context.event])

        # Add global handlers
        handlers_to_notify.extend(self._global_handlers)

        if not handlers_to_notify:
            logger.debug(f"No handlers registered for event {context.event.value}")
            return

        logger.debug(f"Triggering {len(handlers_to_notify)} handlers for event {context.event.value}")

        # Notify all handlers concurrently
        tasks = [handler.handle(context) for handler in handlers_to_notify]

        # Execute all handler tasks, catching and logging any exceptions
        results = await asyncio.gather(*tasks, return_exceptions=True)

        for i, result in enumerate(results):
            if isinstance(result, Exception):
                handler = handlers_to_notify[i]
                logger.error(
                    f"Hook handler {handler.__class__.__name__} failed for event {context.event.value}: {result}",
                    exc_info=result,
                )

    def trigger_sync(self, context: HookContext) -> None:
        """Trigger a hook event synchronously (creates new event loop if needed).

        Args:
            context: The hook context containing event information
        """
        try:
            loop = asyncio.get_event_loop()
            if loop.is_running():
                # If we're already in an event loop, create a task
                asyncio.create_task(self.trigger(context))
            else:
                # Otherwise run in the loop
                loop.run_until_complete(self.trigger(context))
        except RuntimeError:
            # No event loop exists, create a new one
            asyncio.run(self.trigger(context))

    def has_handlers(self, event: HookEvent | None = None) -> bool:
        """Check if there are any handlers registered.

        Args:
            event: Optional event to check for specific handlers

        Returns:
            True if handlers are registered, False otherwise
        """
        if event is None:
            return bool(self._global_handlers or any(self._handlers.values()))
        return bool(self._global_handlers or self._handlers.get(event, []))


# Global hook manager instance
_hook_manager: HookManager | None = None


def get_hook_manager() -> HookManager:
    """Get the global hook manager instance.

    Returns:
        The global HookManager instance
    """
    global _hook_manager
    if _hook_manager is None:
        _hook_manager = HookManager()
    return _hook_manager


def reset_hook_manager() -> None:
    """Reset the global hook manager (primarily for testing)."""
    global _hook_manager
    _hook_manager = None


__all__ = [
    "HookEvent",
    "HookContext",
    "HookHandler",
    "HookManager",
    "get_hook_manager",
    "reset_hook_manager",
]
