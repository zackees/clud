"""Unit tests for the hook system."""

import asyncio
import unittest
from dataclasses import dataclass
from datetime import datetime

from clud.hooks import (
    HookContext,
    HookEvent,
    HookManager,
    get_hook_manager,
    reset_hook_manager,
)


@dataclass
class MockHookHandler:
    """Mock hook handler for testing."""

    name: str
    events_handled: list[HookContext]

    def __post_init__(self) -> None:
        """Initialize events_handled list."""
        if not hasattr(self, "events_handled"):
            self.events_handled = []

    async def handle(self, context: HookContext) -> None:
        """Handle hook event by recording it."""
        self.events_handled.append(context)


@dataclass
class FailingHookHandler:
    """Hook handler that always raises an exception."""

    name: str

    async def handle(self, context: HookContext) -> None:
        """Always raise an exception."""
        raise ValueError(f"Handler {self.name} failed")


class TestHookEvent(unittest.TestCase):
    """Tests for HookEvent enum."""

    def test_hook_event_values(self) -> None:
        """Test that HookEvent has all expected values."""
        self.assertEqual(HookEvent.PRE_EXECUTION.value, "pre_execution")
        self.assertEqual(HookEvent.POST_EXECUTION.value, "post_execution")
        self.assertEqual(HookEvent.OUTPUT_CHUNK.value, "output_chunk")
        self.assertEqual(HookEvent.ERROR.value, "error")
        self.assertEqual(HookEvent.AGENT_START.value, "agent_start")
        self.assertEqual(HookEvent.AGENT_STOP.value, "agent_stop")


class TestHookContext(unittest.TestCase):
    """Tests for HookContext dataclass."""

    def test_hook_context_creation(self) -> None:
        """Test creating a HookContext with required fields."""
        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id="test-instance",
            session_id="test-session",
            client_type="telegram",
            client_id="test-client",
        )

        self.assertEqual(context.event, HookEvent.AGENT_START)
        self.assertEqual(context.instance_id, "test-instance")
        self.assertEqual(context.session_id, "test-session")
        self.assertEqual(context.client_type, "telegram")
        self.assertEqual(context.client_id, "test-client")
        self.assertIsInstance(context.timestamp, datetime)
        self.assertIsNone(context.message)
        self.assertIsNone(context.output)
        self.assertIsNone(context.error)
        self.assertEqual(context.metadata, {})

    def test_hook_context_with_optional_fields(self) -> None:
        """Test creating a HookContext with optional fields."""
        context = HookContext(
            event=HookEvent.OUTPUT_CHUNK,
            instance_id="test-instance",
            session_id="test-session",
            client_type="api",
            client_id="test-client",
            message="test message",
            output="test output",
            error="test error",
            metadata={"key": "value"},
        )

        self.assertEqual(context.message, "test message")
        self.assertEqual(context.output, "test output")
        self.assertEqual(context.error, "test error")
        self.assertEqual(context.metadata, {"key": "value"})


class TestHookManager(unittest.TestCase):
    """Tests for HookManager."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.manager = HookManager()
        self.handler1 = MockHookHandler("handler1", [])
        self.handler2 = MockHookHandler("handler2", [])

    def test_register_handler_for_specific_event(self) -> None:
        """Test registering a handler for specific events."""
        self.manager.register(self.handler1, [HookEvent.AGENT_START])
        self.assertTrue(self.manager.has_handlers(HookEvent.AGENT_START))
        self.assertFalse(self.manager.has_handlers(HookEvent.AGENT_STOP))

    def test_register_handler_for_multiple_events(self) -> None:
        """Test registering a handler for multiple events."""
        self.manager.register(self.handler1, [HookEvent.AGENT_START, HookEvent.AGENT_STOP])
        self.assertTrue(self.manager.has_handlers(HookEvent.AGENT_START))
        self.assertTrue(self.manager.has_handlers(HookEvent.AGENT_STOP))
        self.assertFalse(self.manager.has_handlers(HookEvent.OUTPUT_CHUNK))

    def test_register_global_handler(self) -> None:
        """Test registering a global handler (all events)."""
        self.manager.register(self.handler1, None)
        self.assertTrue(self.manager.has_handlers(HookEvent.AGENT_START))
        self.assertTrue(self.manager.has_handlers(HookEvent.OUTPUT_CHUNK))
        self.assertTrue(self.manager.has_handlers())

    def test_unregister_handler(self) -> None:
        """Test unregistering a handler."""
        self.manager.register(self.handler1, [HookEvent.AGENT_START])
        self.assertTrue(self.manager.has_handlers(HookEvent.AGENT_START))

        self.manager.unregister(self.handler1)
        self.assertFalse(self.manager.has_handlers(HookEvent.AGENT_START))

    def test_unregister_global_handler(self) -> None:
        """Test unregistering a global handler."""
        self.manager.register(self.handler1, None)
        self.assertTrue(self.manager.has_handlers())

        self.manager.unregister(self.handler1)
        self.assertFalse(self.manager.has_handlers())

    def test_trigger_event_with_registered_handler(self) -> None:
        """Test triggering an event with a registered handler."""
        self.manager.register(self.handler1, [HookEvent.AGENT_START])

        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id="test-instance",
            session_id="test-session",
            client_type="api",
            client_id="test-client",
        )

        asyncio.run(self.manager.trigger(context))

        self.assertEqual(len(self.handler1.events_handled), 1)
        self.assertEqual(self.handler1.events_handled[0], context)

    def test_trigger_event_with_no_registered_handlers(self) -> None:
        """Test triggering an event with no registered handlers."""
        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id="test-instance",
            session_id="test-session",
            client_type="api",
            client_id="test-client",
        )

        # Should not raise an exception
        asyncio.run(self.manager.trigger(context))

    def test_trigger_event_with_multiple_handlers(self) -> None:
        """Test triggering an event with multiple registered handlers."""
        self.manager.register(self.handler1, [HookEvent.AGENT_START])
        self.manager.register(self.handler2, [HookEvent.AGENT_START])

        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id="test-instance",
            session_id="test-session",
            client_type="api",
            client_id="test-client",
        )

        asyncio.run(self.manager.trigger(context))

        self.assertEqual(len(self.handler1.events_handled), 1)
        self.assertEqual(len(self.handler2.events_handled), 1)

    def test_trigger_event_with_global_and_specific_handlers(self) -> None:
        """Test triggering with both global and event-specific handlers."""
        self.manager.register(self.handler1, None)  # Global
        self.manager.register(self.handler2, [HookEvent.AGENT_START])  # Specific

        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id="test-instance",
            session_id="test-session",
            client_type="api",
            client_id="test-client",
        )

        asyncio.run(self.manager.trigger(context))

        # Both handlers should be triggered
        self.assertEqual(len(self.handler1.events_handled), 1)
        self.assertEqual(len(self.handler2.events_handled), 1)

    def test_trigger_with_failing_handler(self) -> None:
        """Test that failing handlers don't prevent other handlers from running."""
        failing_handler = FailingHookHandler("failing")
        self.manager.register(failing_handler, [HookEvent.AGENT_START])
        self.manager.register(self.handler1, [HookEvent.AGENT_START])

        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id="test-instance",
            session_id="test-session",
            client_type="api",
            client_id="test-client",
        )

        # Should not raise an exception, handler1 should still be called
        asyncio.run(self.manager.trigger(context))
        self.assertEqual(len(self.handler1.events_handled), 1)

    def test_trigger_sync(self) -> None:
        """Test triggering an event synchronously."""
        self.manager.register(self.handler1, [HookEvent.AGENT_START])

        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id="test-instance",
            session_id="test-session",
            client_type="api",
            client_id="test-client",
        )

        self.manager.trigger_sync(context)

        self.assertEqual(len(self.handler1.events_handled), 1)

    def test_has_handlers_no_event(self) -> None:
        """Test has_handlers with no specific event."""
        self.assertFalse(self.manager.has_handlers())

        self.manager.register(self.handler1, [HookEvent.AGENT_START])
        self.assertTrue(self.manager.has_handlers())


class TestGlobalHookManager(unittest.TestCase):
    """Tests for global hook manager functions."""

    def setUp(self) -> None:
        """Reset global hook manager before each test."""
        reset_hook_manager()

    def tearDown(self) -> None:
        """Reset global hook manager after each test."""
        reset_hook_manager()

    def test_get_hook_manager_singleton(self) -> None:
        """Test that get_hook_manager returns a singleton."""
        manager1 = get_hook_manager()
        manager2 = get_hook_manager()
        self.assertIs(manager1, manager2)

    def test_reset_hook_manager(self) -> None:
        """Test that reset_hook_manager creates a new instance."""
        manager1 = get_hook_manager()
        reset_hook_manager()
        manager2 = get_hook_manager()
        self.assertIsNot(manager1, manager2)

    def test_global_hook_manager_registration(self) -> None:
        """Test registering handlers with global hook manager."""
        manager = get_hook_manager()
        handler = MockHookHandler("global-handler", [])

        manager.register(handler, [HookEvent.AGENT_START])
        self.assertTrue(manager.has_handlers(HookEvent.AGENT_START))

        # Verify same manager instance has the registration
        manager2 = get_hook_manager()
        self.assertTrue(manager2.has_handlers(HookEvent.AGENT_START))


if __name__ == "__main__":
    unittest.main()
