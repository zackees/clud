"""Integration tests for TelegramHookHandler with FakeTelegramBotAPI.

This test suite validates that TelegramHookHandler correctly integrates with
the abstract TelegramBotAPI interface using the FakeTelegramBotAPI implementation.
Tests cover event forwarding, buffering, message splitting, and error handling.
"""

import asyncio
import unittest
from unittest.mock import AsyncMock

from clud.hooks import HookContext, HookEvent
from clud.hooks.telegram import TELEGRAM_MAX_MESSAGE_LENGTH, TelegramHookHandler
from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_fake import FakeTelegramBotAPI


class TestTelegramHookHandlerIntegration(unittest.IsolatedAsyncioTestCase):
    """Integration test cases for TelegramHookHandler with FakeTelegramBotAPI."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create fake API with zero delay for fast tests
        config = TelegramAPIConfig.for_testing(implementation="fake")
        self.fake_api = FakeTelegramBotAPI(config)

        # Test session and chat data
        self.test_chat_id = "123456789"
        self.test_session_id = self.test_chat_id
        self.test_instance_id = "test-instance-12345678"

    async def asyncSetUp(self) -> None:
        """Async setup - initialize fake API."""
        await self.fake_api.initialize()

    async def asyncTearDown(self) -> None:
        """Async teardown - clean up fake API."""
        self.fake_api.clear_history()
        await self.fake_api.shutdown()

    def _create_hook_context(
        self,
        event: HookEvent,
        output: str | None = None,
        error: str | None = None,
    ) -> HookContext:
        """Helper to create HookContext for testing.

        Args:
            event: Hook event type
            output: Optional output content
            error: Optional error message

        Returns:
            HookContext instance
        """
        return HookContext(
            event=event,
            instance_id=self.test_instance_id,
            session_id=self.test_session_id,
            client_type="telegram",
            client_id=self.test_chat_id,
            output=output,
            error=error,
        )

    # Test AGENT_START event

    async def test_agent_start_sends_welcome_message(self) -> None:
        """Test that AGENT_START event sends a welcome message."""
        # Setup
        handler = TelegramHookHandler(bot_token="fake-token", api=self.fake_api)
        context = self._create_hook_context(HookEvent.AGENT_START)

        # Execute
        await handler.handle(context)

        # Verify
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Agent Started", messages[0].text)
        self.assertIn(self.test_session_id, messages[0].text)
        self.assertIn(self.test_instance_id[:8], messages[0].text)

    # Test OUTPUT_CHUNK event and buffering

    async def test_output_chunk_buffers_small_output(self) -> None:
        """Test that OUTPUT_CHUNK buffers output without immediate sending."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,  # Long delay to test buffering
        )
        context = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Small output")

        # Execute
        await handler.handle(context)

        # Verify - should be buffered, not sent yet
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 0)

    async def test_output_chunk_flushes_when_buffer_full(self) -> None:
        """Test that OUTPUT_CHUNK flushes buffer when it reaches buffer_size."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=50,  # Small buffer for testing
            flush_interval=10.0,
        )

        # Create output that exceeds buffer size
        large_output = "x" * 60

        context = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output=large_output)

        # Execute
        await handler.handle(context)

        # Verify - should be flushed immediately
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn(large_output, messages[0].text)

    async def test_output_chunk_accumulates_multiple_chunks(self) -> None:
        """Test that multiple OUTPUT_CHUNK events accumulate in buffer."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Execute - send multiple chunks
        for i in range(3):
            context = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output=f"Chunk {i}\n")
            await handler.handle(context)

        # Verify - should still be buffered
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 0)

        # Now flush manually by triggering POST_EXECUTION
        context = self._create_hook_context(HookEvent.POST_EXECUTION)
        await handler.handle(context)

        # Verify - all chunks should be in the flushed message
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        # Should have 2 messages: flushed output + completion message
        self.assertGreaterEqual(len(messages), 1)
        output_message = messages[0]
        self.assertIn("Chunk 0", output_message.text)
        self.assertIn("Chunk 1", output_message.text)
        self.assertIn("Chunk 2", output_message.text)

    async def test_output_chunk_delayed_flush(self) -> None:
        """Test that OUTPUT_CHUNK flushes after flush_interval timeout."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=0.1,  # 100ms delay
        )
        context = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Test output")

        # Execute
        await handler.handle(context)

        # Verify - should be buffered initially
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 0)

        # Wait for flush interval
        await asyncio.sleep(0.2)

        # Verify - should be flushed now
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Test output", messages[0].text)

    async def test_output_chunk_cancels_previous_flush_task(self) -> None:
        """Test that new OUTPUT_CHUNK cancels previous delayed flush task."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=0.2,  # 200ms delay
        )

        # Execute - send first chunk
        context1 = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="First ")
        await handler.handle(context1)

        # Wait a bit but not long enough to flush
        await asyncio.sleep(0.1)

        # Execute - send second chunk (should cancel first flush task)
        context2 = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Second")
        await handler.handle(context2)

        # Wait for second flush interval
        await asyncio.sleep(0.25)

        # Verify - should have one message with both chunks
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("First", messages[0].text)
        self.assertIn("Second", messages[0].text)

    # Test POST_EXECUTION event

    async def test_post_execution_flushes_buffer(self) -> None:
        """Test that POST_EXECUTION flushes buffered output."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Buffer some output
        context_output = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Buffered output")
        await handler.handle(context_output)

        # Execute POST_EXECUTION
        context_post = self._create_hook_context(HookEvent.POST_EXECUTION)
        await handler.handle(context_post)

        # Verify - should have 2 messages: flushed output + completion message
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertGreaterEqual(len(messages), 2)
        self.assertIn("Buffered output", messages[0].text)
        self.assertIn("Execution Complete", messages[1].text)

    async def test_post_execution_sends_completion_message(self) -> None:
        """Test that POST_EXECUTION sends completion message."""
        # Setup
        handler = TelegramHookHandler(bot_token="fake-token", api=self.fake_api)
        context = self._create_hook_context(HookEvent.POST_EXECUTION)

        # Execute
        await handler.handle(context)

        # Verify
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Execution Complete", messages[0].text)
        self.assertIn(self.test_instance_id[:8], messages[0].text)

    # Test ERROR event

    async def test_error_flushes_buffer_and_sends_error(self) -> None:
        """Test that ERROR event flushes buffer and sends error message."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Buffer some output
        context_output = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Output before error")
        await handler.handle(context_output)

        # Execute ERROR event
        context_error = self._create_hook_context(HookEvent.ERROR, error="Test error message")
        await handler.handle(context_error)

        # Verify - should have 2 messages: flushed output + error message
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertGreaterEqual(len(messages), 2)
        self.assertIn("Output before error", messages[0].text)
        self.assertIn("Error Occurred", messages[1].text)
        self.assertIn("Test error message", messages[1].text)

    async def test_error_truncates_long_error_messages(self) -> None:
        """Test that ERROR event truncates very long error messages."""
        # Setup
        handler = TelegramHookHandler(bot_token="fake-token", api=self.fake_api)

        # Create long error message
        long_error = "Error: " + ("x" * 2000)
        context = self._create_hook_context(HookEvent.ERROR, error=long_error)

        # Execute
        await handler.handle(context)

        # Verify - error should be truncated
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Error Occurred", messages[0].text)
        self.assertIn("(truncated)", messages[0].text)
        # Verify it's not the full length
        self.assertLess(len(messages[0].text), len(long_error))

    # Test AGENT_STOP event

    async def test_agent_stop_flushes_buffer_and_sends_stop_message(self) -> None:
        """Test that AGENT_STOP flushes buffer and sends stop message."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Buffer some output
        context_output = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Final output")
        await handler.handle(context_output)

        # Execute AGENT_STOP
        context_stop = self._create_hook_context(HookEvent.AGENT_STOP)
        await handler.handle(context_stop)

        # Verify - should have 2 messages: flushed output + stop message
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertGreaterEqual(len(messages), 2)
        self.assertIn("Final output", messages[0].text)
        self.assertIn("Agent Stopped", messages[1].text)

    # Test PRE_EXECUTION event (should be no-op for telegram handler)

    async def test_pre_execution_is_ignored(self) -> None:
        """Test that PRE_EXECUTION event is ignored (no action taken)."""
        # Setup
        handler = TelegramHookHandler(bot_token="fake-token", api=self.fake_api)
        context = self._create_hook_context(HookEvent.PRE_EXECUTION)

        # Execute
        await handler.handle(context)

        # Verify - no messages should be sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 0)

    # Test non-telegram client filtering

    async def test_ignores_non_telegram_client_events(self) -> None:
        """Test that handler ignores events from non-telegram clients."""
        # Setup
        handler = TelegramHookHandler(bot_token="fake-token", api=self.fake_api)

        # Create context with different client_type
        context = HookContext(
            event=HookEvent.AGENT_START,
            instance_id=self.test_instance_id,
            session_id=self.test_session_id,
            client_type="webhook",  # Not telegram
            client_id="webhook-123",
        )

        # Execute
        await handler.handle(context)

        # Verify - no messages should be sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 0)

    # Test message splitting

    async def test_message_splitting_for_long_output(self) -> None:
        """Test that very long messages are split into chunks."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=TELEGRAM_MAX_MESSAGE_LENGTH + 100,  # Allow large buffer
            flush_interval=10.0,
        )

        # Create very long output that exceeds Telegram limit
        # Each line is 50 chars, need enough to exceed 2000 char limit
        lines = [f"Line {i:04d}: " + ("x" * 40) for i in range(50)]
        long_output = "\n".join(lines)

        # Buffer and flush the long output
        context_output = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output=long_output)
        await handler.handle(context_output)

        context_flush = self._create_hook_context(HookEvent.POST_EXECUTION)
        await handler.handle(context_flush)

        # Verify - should have split into multiple messages
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        # At least 2 messages (split output) + 1 completion message
        self.assertGreaterEqual(len(messages), 2)

        # Verify first message contains early lines
        self.assertIn("Line 0000", messages[0].text)

        # Verify all lines are present across messages
        all_text = "".join(msg.text for msg in messages)
        for line in lines[:10]:  # Check first 10 lines
            self.assertIn(line.split(":")[0], all_text)

    # Test send_callback fallback

    async def test_send_callback_fallback(self) -> None:
        """Test that handler can use send_callback instead of API."""
        # Setup - track callback calls
        callback_calls: list[tuple[str, str]] = []

        async def mock_callback(chat_id: str, text: str) -> None:
            callback_calls.append((chat_id, text))

        handler = TelegramHookHandler(
            bot_token="fake-token",
            send_callback=mock_callback,
            api=None,  # No API provided
        )

        context = self._create_hook_context(HookEvent.AGENT_START)

        # Execute
        await handler.handle(context)

        # Verify - callback should be called
        self.assertEqual(len(callback_calls), 1)
        chat_id, text = callback_calls[0]
        self.assertEqual(chat_id, self.test_session_id)
        self.assertIn("Agent Started", text)

    # Test error handling in message sending

    async def test_error_in_send_message_is_logged(self) -> None:
        """Test that errors in send_message are caught and logged."""
        # Setup - create API that will fail
        failing_api = AsyncMock(spec=FakeTelegramBotAPI)
        failing_api.send_message.side_effect = Exception("Network error")

        handler = TelegramHookHandler(bot_token="fake-token", api=failing_api)
        context = self._create_hook_context(HookEvent.AGENT_START)

        # Execute - should not raise exception
        try:
            await handler.handle(context)
        except Exception as e:
            self.fail(f"Handler should catch and log errors, not raise them: {e}")

        # Verify - send_message was attempted
        failing_api.send_message.assert_called_once()

    # Test output formatting

    async def test_output_formatting_with_code_blocks(self) -> None:
        """Test that output is formatted in code blocks."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Buffer and flush output
        context_output = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="test output")
        await handler.handle(context_output)

        context_flush = self._create_hook_context(HookEvent.POST_EXECUTION)
        await handler.handle(context_flush)

        # Verify - output should be wrapped in code blocks
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        output_message = messages[0]
        self.assertIn("```", output_message.text)
        self.assertIn("test output", output_message.text)

    # Test empty output handling

    async def test_empty_output_is_skipped(self) -> None:
        """Test that empty output chunks are not buffered."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Send empty output
        context_empty = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="")
        await handler.handle(context_empty)

        # Send None output
        context_none = self._create_hook_context(HookEvent.OUTPUT_CHUNK, output=None)
        await handler.handle(context_none)

        # Flush
        context_flush = self._create_hook_context(HookEvent.POST_EXECUTION)
        await handler.handle(context_flush)

        # Verify - only completion message, no output message
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn("Execution Complete", messages[0].text)

    # Integration flow tests

    async def test_full_execution_flow(self) -> None:
        """Test complete execution flow: start -> output -> complete."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Execute full flow
        await handler.handle(self._create_hook_context(HookEvent.AGENT_START))
        await handler.handle(self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Step 1"))
        await handler.handle(self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Step 2"))
        await handler.handle(self._create_hook_context(HookEvent.POST_EXECUTION))

        # Verify - should have 3 messages: start, output, completion
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 3)
        self.assertIn("Agent Started", messages[0].text)
        self.assertIn("Step 1", messages[1].text)
        self.assertIn("Step 2", messages[1].text)
        self.assertIn("Execution Complete", messages[2].text)

    async def test_error_flow_with_output(self) -> None:
        """Test error flow: start -> output -> error."""
        # Setup
        handler = TelegramHookHandler(
            bot_token="fake-token",
            api=self.fake_api,
            buffer_size=1000,
            flush_interval=10.0,
        )

        # Execute error flow
        await handler.handle(self._create_hook_context(HookEvent.AGENT_START))
        await handler.handle(self._create_hook_context(HookEvent.OUTPUT_CHUNK, output="Working..."))
        await handler.handle(self._create_hook_context(HookEvent.ERROR, error="Something failed"))

        # Verify - should have 3 messages: start, output, error
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 3)
        self.assertIn("Agent Started", messages[0].text)
        self.assertIn("Working...", messages[1].text)
        self.assertIn("Error Occurred", messages[2].text)
        self.assertIn("Something failed", messages[2].text)


if __name__ == "__main__":
    unittest.main()
