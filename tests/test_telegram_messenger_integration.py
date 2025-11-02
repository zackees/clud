"""Integration tests for TelegramMessenger with FakeTelegramBotAPI."""

import asyncio
import unittest

from clud.messaging.telegram import TelegramMessenger
from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_fake import FakeTelegramBotAPI


class TestTelegramMessengerIntegration(unittest.IsolatedAsyncioTestCase):
    """Integration test cases for TelegramMessenger with FakeTelegramBotAPI."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        # Create TelegramAPIConfig for testing (zero delay for fast tests)
        self.api_config = TelegramAPIConfig.for_testing(implementation="fake")

        # Create FakeTelegramBotAPI
        self.fake_api = FakeTelegramBotAPI(config=self.api_config)

        # Test chat ID
        self.test_chat_id = "12345"

        # Test data
        self.agent_name = "test-agent"
        self.process_id = "pid-12345"
        self.metadata = {
            "project_path": "/path/to/project",
            "mode": "foreground",
        }
        self.status_message = "Processing user request"
        self.summary = {
            "duration": "5m 30s",
            "tasks_completed": 10,
            "files_modified": 5,
            "error_count": 0,
        }

    async def asyncSetUp(self) -> None:
        """Async setup - initialize fake API."""
        await self.fake_api.initialize()

    async def asyncTearDown(self) -> None:
        """Async teardown - clear history and shutdown fake API."""
        self.fake_api.clear_history()
        await self.fake_api.shutdown()

    # ==================== Initialization Tests ====================

    async def test_initialization_with_api(self) -> None:
        """Test messenger initializes correctly with API parameter."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Should initialize successfully with API
        result = await messenger._ensure_initialized()
        self.assertTrue(result)
        self.assertTrue(messenger._initialized)
        self.assertEqual(messenger.bot_token, "fake-token")
        self.assertEqual(messenger.chat_id, self.test_chat_id)

    async def test_initialization_without_api(self) -> None:
        """Test messenger initialization without API parameter (fallback mode)."""
        # Note: Without python-telegram-bot installed, this should fail gracefully
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=None,  # No API provided
        )

        # Initialization will fail because python-telegram-bot may not be available
        # or because it's a fake token, but should not raise an exception
        result = await messenger._ensure_initialized()
        # Result can be True or False depending on python-telegram-bot availability
        # We just verify it doesn't crash
        self.assertIsInstance(result, bool)

    # ==================== Invitation Message Tests ====================

    async def test_send_invitation(self) -> None:
        """Test sending invitation message."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        result = await messenger.send_invitation(
            agent_name=self.agent_name,
            process_id=self.process_id,
            metadata=self.metadata,
        )

        self.assertTrue(result)

        # Verify message was sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)

        message_text = messages[0].text
        # Verify message contains expected content
        self.assertIn("Claude Agent Launched", message_text)
        self.assertIn(self.agent_name, message_text)
        self.assertIn(self.process_id, message_text)
        self.assertIn(self.metadata["project_path"], message_text)
        self.assertIn(self.metadata["mode"], message_text)
        self.assertIn("Online and ready", message_text)
        self.assertIn("🚀", message_text)  # Emoji

        # Verify parse mode
        self.assertEqual(messages[0].parse_mode, "Markdown")

    async def test_send_invitation_with_minimal_metadata(self) -> None:
        """Test sending invitation with minimal metadata."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        minimal_metadata: dict[str, str] = {}  # Empty metadata

        result = await messenger.send_invitation(
            agent_name=self.agent_name,
            process_id=self.process_id,
            metadata=minimal_metadata,
        )

        self.assertTrue(result)

        # Verify message was sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)

        message_text = messages[0].text
        # Should still contain agent name and process ID
        self.assertIn(self.agent_name, message_text)
        self.assertIn(self.process_id, message_text)
        # Should show "N/A" for missing fields
        self.assertIn("N/A", message_text)

    # ==================== Status Update Tests ====================

    async def test_send_status_update(self) -> None:
        """Test sending status update message."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        result = await messenger.send_status_update(
            agent_name=self.agent_name,
            status=self.status_message,
            details=None,
        )

        self.assertTrue(result)

        # Verify message was sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)

        message_text = messages[0].text
        # Verify message contains expected content
        self.assertIn("Agent Status Update", message_text)
        self.assertIn(self.agent_name, message_text)
        self.assertIn(self.status_message, message_text)
        self.assertIn("📊", message_text)  # Emoji

        # Verify parse mode
        self.assertEqual(messages[0].parse_mode, "Markdown")

    async def test_send_status_update_with_details(self) -> None:
        """Test sending status update with additional details."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        details = {
            "current_task": "Analyzing code",
            "progress": "50%",
            "estimated_time": "2 minutes",
        }

        result = await messenger.send_status_update(
            agent_name=self.agent_name,
            status=self.status_message,
            details=details,
        )

        self.assertTrue(result)

        # Verify message was sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)

        message_text = messages[0].text
        # Verify message contains status and all details
        self.assertIn(self.status_message, message_text)
        self.assertIn("Details:", message_text)
        for key, value in details.items():
            self.assertIn(key, message_text)
            self.assertIn(value, message_text)

    # ==================== Cleanup Notification Tests ====================

    async def test_send_cleanup_notification(self) -> None:
        """Test sending cleanup notification."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        result = await messenger.send_cleanup_notification(
            agent_name=self.agent_name,
            summary=self.summary,
        )

        self.assertTrue(result)

        # Verify message was sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)

        message_text = messages[0].text
        # Verify message contains expected content
        self.assertIn("Agent Cleanup Complete", message_text)
        self.assertIn(self.agent_name, message_text)
        self.assertIn(str(self.summary["duration"]), message_text)
        self.assertIn(str(self.summary["tasks_completed"]), message_text)
        self.assertIn(str(self.summary["files_modified"]), message_text)
        self.assertIn(str(self.summary["error_count"]), message_text)
        self.assertIn("Offline", message_text)
        self.assertIn("✅", message_text)  # Emoji

        # Verify parse mode
        self.assertEqual(messages[0].parse_mode, "Markdown")

    async def test_send_cleanup_notification_with_partial_summary(self) -> None:
        """Test cleanup notification with partial summary data."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        partial_summary: dict[str, int | str] = {
            "duration": "3m 15s",
            # Missing other fields - should use defaults
        }

        result = await messenger.send_cleanup_notification(
            agent_name=self.agent_name,
            summary=partial_summary,
        )

        self.assertTrue(result)

        # Verify message was sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)

        message_text = messages[0].text
        # Should show duration and defaults for other fields
        self.assertIn("3m 15s", message_text)
        # Missing numeric fields default to 0
        self.assertIn("Tasks Completed**: 0", message_text)
        self.assertIn("Files Modified**: 0", message_text)
        self.assertIn("Errors**: 0", message_text)

    # ==================== Message Queue Tests ====================

    async def test_receive_message_timeout(self) -> None:
        """Test receiving message with timeout (no message available)."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Try to receive with very short timeout
        result = await messenger.receive_message(timeout=1)

        # Should timeout and return None
        self.assertIsNone(result)

    async def test_receive_message_from_queue(self) -> None:
        """Test receiving message that was placed in queue."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Ensure initialized
        await messenger._ensure_initialized()

        # Manually put a message in the queue
        test_message = "Hello from user!"
        await messenger.message_queue.put(test_message)

        # Receive the message
        result = await messenger.receive_message(timeout=1)

        self.assertEqual(result, test_message)

    # ==================== Integration Flow Tests ====================

    async def test_full_lifecycle_flow(self) -> None:
        """Test complete agent lifecycle: invitation -> status -> cleanup."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Step 1: Send invitation
        result1 = await messenger.send_invitation(
            agent_name=self.agent_name,
            process_id=self.process_id,
            metadata=self.metadata,
        )
        self.assertTrue(result1)

        # Step 2: Send status update
        result2 = await messenger.send_status_update(
            agent_name=self.agent_name,
            status="Working on task 1",
            details={"progress": "25%"},
        )
        self.assertTrue(result2)

        # Step 3: Send another status update
        result3 = await messenger.send_status_update(
            agent_name=self.agent_name,
            status="Working on task 2",
            details={"progress": "75%"},
        )
        self.assertTrue(result3)

        # Step 4: Send cleanup notification
        result4 = await messenger.send_cleanup_notification(
            agent_name=self.agent_name,
            summary=self.summary,
        )
        self.assertTrue(result4)

        # Verify all messages were sent in order
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 4)

        # Verify message order and content
        self.assertIn("Claude Agent Launched", messages[0].text)
        self.assertIn("Working on task 1", messages[1].text)
        self.assertIn("Working on task 2", messages[2].text)
        self.assertIn("Agent Cleanup Complete", messages[3].text)

    async def test_multiple_status_updates(self) -> None:
        """Test sending multiple consecutive status updates."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        statuses = [
            "Analyzing requirements",
            "Writing code",
            "Running tests",
            "Generating documentation",
            "Committing changes",
        ]

        # Send multiple status updates
        for status in statuses:
            result = await messenger.send_status_update(
                agent_name=self.agent_name,
                status=status,
                details=None,
            )
            self.assertTrue(result)

        # Verify all messages sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), len(statuses))

        # Verify each status appears in order
        for i, status in enumerate(statuses):
            self.assertIn(status, messages[i].text)

    # ==================== Error Handling Tests ====================

    async def test_send_invitation_with_api_failure(self) -> None:
        """Test invitation sending when API send_message fails."""
        from unittest.mock import AsyncMock

        # Create messenger with mocked API that fails
        mock_api = AsyncMock()
        mock_api.send_message = AsyncMock(side_effect=Exception("Network error"))
        mock_api.initialize = AsyncMock(return_value=True)

        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=mock_api,
        )

        result = await messenger.send_invitation(
            agent_name=self.agent_name,
            process_id=self.process_id,
            metadata=self.metadata,
        )

        # Should return False on failure
        self.assertFalse(result)

        # Verify send_message was called
        mock_api.send_message.assert_called_once()

    async def test_send_status_update_with_api_failure(self) -> None:
        """Test status update when API send_message fails."""
        from unittest.mock import AsyncMock

        # Create messenger with mocked API that fails
        mock_api = AsyncMock()
        mock_api.send_message = AsyncMock(side_effect=Exception("Network error"))
        mock_api.initialize = AsyncMock(return_value=True)

        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=mock_api,
        )

        result = await messenger.send_status_update(
            agent_name=self.agent_name,
            status=self.status_message,
            details=None,
        )

        # Should return False on failure
        self.assertFalse(result)

        # Verify send_message was called
        mock_api.send_message.assert_called_once()

    async def test_send_cleanup_notification_with_api_failure(self) -> None:
        """Test cleanup notification when API send_message fails."""
        from unittest.mock import AsyncMock

        # Create messenger with mocked API that fails
        mock_api = AsyncMock()
        mock_api.send_message = AsyncMock(side_effect=Exception("Network error"))
        mock_api.initialize = AsyncMock(return_value=True)

        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=mock_api,
        )

        result = await messenger.send_cleanup_notification(
            agent_name=self.agent_name,
            summary=self.summary,
        )

        # Should return False on failure
        self.assertFalse(result)

        # Verify send_message was called
        mock_api.send_message.assert_called_once()

    # ==================== Edge Case Tests ====================

    async def test_empty_agent_name(self) -> None:
        """Test messages with empty agent name."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        result = await messenger.send_invitation(
            agent_name="",  # Empty name
            process_id=self.process_id,
            metadata=self.metadata,
        )

        self.assertTrue(result)

        # Verify message still sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)

    async def test_special_characters_in_messages(self) -> None:
        """Test messages with special markdown characters."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        special_status = "Processing `code` and **bold** text with _italic_"

        result = await messenger.send_status_update(
            agent_name=self.agent_name,
            status=special_status,
            details=None,
        )

        self.assertTrue(result)

        # Verify message sent with special characters
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        self.assertIn(special_status, messages[0].text)

    async def test_long_message_content(self) -> None:
        """Test sending messages with long content."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Create long status message
        long_status = "Processing request " + "with many details " * 50

        result = await messenger.send_status_update(
            agent_name=self.agent_name,
            status=long_status,
            details=None,
        )

        self.assertTrue(result)

        # Verify message sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 1)
        # Content should be in the message
        self.assertIn("Processing request", messages[0].text)

    async def test_concurrent_message_sends(self) -> None:
        """Test sending multiple messages concurrently."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Send multiple messages concurrently
        tasks = [messenger.send_status_update(self.agent_name, f"Status {i}", None) for i in range(5)]

        results = await asyncio.gather(*tasks)

        # All should succeed
        self.assertTrue(all(results))

        # Verify all messages sent
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 5)

    # ==================== Markdown Formatting Tests ====================

    async def test_markdown_formatting_preserved(self) -> None:
        """Test that Markdown formatting is preserved in messages."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Send invitation and check formatting
        await messenger.send_invitation(
            agent_name=self.agent_name,
            process_id=self.process_id,
            metadata=self.metadata,
        )

        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        message_text = messages[0].text

        # Verify Markdown formatting elements are present
        self.assertIn("**", message_text)  # Bold markers
        self.assertIn("`", message_text)  # Code markers

    async def test_all_message_types_use_markdown(self) -> None:
        """Test that all message types use Markdown parse mode."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Send one of each message type
        await messenger.send_invitation(self.agent_name, self.process_id, self.metadata)
        await messenger.send_status_update(self.agent_name, self.status_message, None)
        await messenger.send_cleanup_notification(self.agent_name, self.summary)

        # Verify all use Markdown
        messages = self.fake_api.get_sent_messages(int(self.test_chat_id))
        self.assertEqual(len(messages), 3)

        for message in messages:
            self.assertEqual(message.parse_mode, "Markdown")

    # ==================== Reinitialization Tests ====================

    async def test_multiple_ensure_initialized_calls(self) -> None:
        """Test that multiple _ensure_initialized calls don't reinitialize."""
        messenger = TelegramMessenger(
            bot_token="fake-token",
            chat_id=self.test_chat_id,
            api=self.fake_api,
        )

        # Call multiple times
        result1 = await messenger._ensure_initialized()
        result2 = await messenger._ensure_initialized()
        result3 = await messenger._ensure_initialized()

        # All should succeed
        self.assertTrue(result1)
        self.assertTrue(result2)
        self.assertTrue(result3)

        # Messenger should be marked as initialized
        self.assertTrue(messenger._initialized)


if __name__ == "__main__":
    unittest.main()
