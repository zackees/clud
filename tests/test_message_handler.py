"""Unit tests for MessageHandler."""

import asyncio
import unittest
from datetime import datetime
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

from clud.api.message_handler import MessageHandler
from clud.api.models import ClientType, ExecutionStatus, MessageRequest


class TestMessageHandler(unittest.TestCase):
    """Tests for MessageHandler class."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.handler = MessageHandler(max_instances=10, idle_timeout_seconds=300)

    def tearDown(self) -> None:
        """Clean up after tests."""
        # Shutdown handler to clean up resources
        asyncio.run(self.handler.shutdown())

    def test_message_handler_initialization(self) -> None:
        """Test MessageHandler initialization."""
        handler = MessageHandler(max_instances=5, idle_timeout_seconds=600)
        self.assertIsNotNone(handler._instance_pool)

    def test_handle_message_with_invalid_request(self) -> None:
        """Test handling an invalid message request."""

        async def run_test() -> None:
            # Create request with empty message
            request = MessageRequest(
                message="",
                session_id="session-123",
                client_type=ClientType.API,
                client_id="client-456",
            )

            response = await self.handler.handle_message(request)

            self.assertEqual(response.session_id, "session-123")
            self.assertEqual(response.status, ExecutionStatus.FAILED)
            self.assertIsNotNone(response.error)
            self.assertEqual(response.instance_id, "")

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool.get_or_create_instance")
    def test_handle_message_creates_instance(self, mock_get_or_create: MagicMock) -> None:
        """Test that handle_message creates an instance for a new session."""

        async def run_test() -> None:
            # Create mock instance
            mock_instance = MagicMock()
            mock_instance.instance_id = "instance-123"
            mock_instance.message_count = 1
            mock_instance.execute = AsyncMock(
                return_value={
                    "status": "completed",
                    "output": "test output",
                    "error": None,
                    "exit_code": 0,
                }
            )
            mock_instance.to_instance_info = MagicMock(
                return_value=MagicMock(
                    instance_id="instance-123",
                    session_id="session-456",
                    client_type=ClientType.API,
                    client_id="client-789",
                    status=ExecutionStatus.RUNNING,
                    created_at=datetime.now(),
                    last_activity=datetime.now(),
                )
            )

            mock_get_or_create.return_value = mock_instance

            # Create valid request
            request = MessageRequest(
                message="test command",
                session_id="session-456",
                client_type=ClientType.API,
                client_id="client-789",
                working_directory="/home/user/project",
            )

            response = await self.handler.handle_message(request)

            # Verify instance creation was called
            mock_get_or_create.assert_called_once_with(
                session_id="session-456",
                client_type=ClientType.API,
                client_id="client-789",
                working_directory=Path("/home/user/project"),
            )

            # Verify response
            self.assertEqual(response.instance_id, "instance-123")
            self.assertEqual(response.session_id, "session-456")
            self.assertEqual(response.status, ExecutionStatus.COMPLETED)
            self.assertEqual(response.message, "test output")
            self.assertIsNone(response.error)

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool.get_or_create_instance")
    def test_handle_message_executes_command(self, mock_get_or_create: MagicMock) -> None:
        """Test that handle_message executes the command on the instance."""

        async def run_test() -> None:
            # Create mock instance
            mock_instance = MagicMock()
            mock_instance.instance_id = "instance-123"
            mock_instance.message_count = 1
            mock_instance.execute = AsyncMock(
                return_value={
                    "status": "completed",
                    "output": "command executed successfully",
                    "error": None,
                    "exit_code": 0,
                }
            )
            mock_instance.to_instance_info = MagicMock(
                return_value=MagicMock(
                    instance_id="instance-123",
                    session_id="session-456",
                    client_type=ClientType.API,
                    client_id="client-789",
                    status=ExecutionStatus.RUNNING,
                    created_at=datetime.now(),
                    last_activity=datetime.now(),
                )
            )

            mock_get_or_create.return_value = mock_instance

            # Create request
            request = MessageRequest(
                message="echo hello",
                session_id="session-456",
                client_type=ClientType.API,
                client_id="client-789",
            )

            response = await self.handler.handle_message(request)

            # Verify execute was called with the message
            mock_instance.execute.assert_called_once_with("echo hello")

            # Verify response
            self.assertEqual(response.status, ExecutionStatus.COMPLETED)
            self.assertEqual(response.message, "command executed successfully")

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool.get_or_create_instance")
    def test_handle_message_handles_execution_failure(self, mock_get_or_create: MagicMock) -> None:
        """Test that handle_message properly handles execution failures."""

        async def run_test() -> None:
            # Create mock instance that returns a failed execution
            mock_instance = MagicMock()
            mock_instance.instance_id = "instance-123"
            mock_instance.message_count = 1
            mock_instance.execute = AsyncMock(
                return_value={
                    "status": "failed",
                    "output": "partial output",
                    "error": "command failed",
                    "exit_code": 1,
                }
            )
            mock_instance.to_instance_info = MagicMock(
                return_value=MagicMock(
                    instance_id="instance-123",
                    session_id="session-456",
                    client_type=ClientType.API,
                    client_id="client-789",
                    status=ExecutionStatus.RUNNING,
                    created_at=datetime.now(),
                    last_activity=datetime.now(),
                )
            )

            mock_get_or_create.return_value = mock_instance

            # Create request
            request = MessageRequest(
                message="invalid command",
                session_id="session-456",
                client_type=ClientType.API,
                client_id="client-789",
            )

            response = await self.handler.handle_message(request)

            # Verify response indicates failure
            self.assertEqual(response.status, ExecutionStatus.FAILED)
            self.assertEqual(response.message, "partial output")
            self.assertEqual(response.error, "command failed")
            self.assertEqual(response.metadata["exit_code"], 1)

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool.get_or_create_instance")
    def test_handle_message_handles_exception(self, mock_get_or_create: MagicMock) -> None:
        """Test that handle_message handles exceptions gracefully."""

        async def run_test() -> None:
            # Make get_or_create_instance raise an exception
            mock_get_or_create.side_effect = RuntimeError("Instance creation failed")

            # Create request
            request = MessageRequest(
                message="test command",
                session_id="session-456",
                client_type=ClientType.API,
                client_id="client-789",
            )

            response = await self.handler.handle_message(request)

            # Verify response indicates failure
            self.assertEqual(response.status, ExecutionStatus.FAILED)
            self.assertIn("Internal error", response.error or "")
            self.assertEqual(response.instance_id, "")

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool")
    def test_get_instance(self, mock_pool_class: MagicMock) -> None:
        """Test getting an instance by ID."""
        # Create mock instance
        mock_instance = MagicMock()
        mock_instance.to_instance_info = MagicMock(return_value=MagicMock(instance_id="instance-123"))

        # Setup mock pool
        mock_pool = MagicMock()
        mock_pool.get_instance.return_value = mock_instance
        mock_pool_class.return_value = mock_pool

        handler = MessageHandler()
        handler._instance_pool = mock_pool

        result = handler.get_instance("instance-123")

        mock_pool.get_instance.assert_called_once_with("instance-123")
        self.assertIsNotNone(result)
        self.assertEqual(result.instance_id, "instance-123")

    @patch("clud.api.instance_manager.InstancePool")
    def test_get_instance_not_found(self, mock_pool_class: MagicMock) -> None:
        """Test getting a non-existent instance."""
        # Setup mock pool to return None
        mock_pool = MagicMock()
        mock_pool.get_instance.return_value = None
        mock_pool_class.return_value = mock_pool

        handler = MessageHandler()
        handler._instance_pool = mock_pool

        result = handler.get_instance("nonexistent")

        self.assertIsNone(result)

    @patch("clud.api.instance_manager.InstancePool")
    def test_get_session_instance(self, mock_pool_class: MagicMock) -> None:
        """Test getting an instance by session ID."""
        # Create mock instance
        mock_instance = MagicMock()
        mock_instance.to_instance_info = MagicMock(return_value=MagicMock(session_id="session-123"))

        # Setup mock pool
        mock_pool = MagicMock()
        mock_pool.get_session_instance.return_value = mock_instance
        mock_pool_class.return_value = mock_pool

        handler = MessageHandler()
        handler._instance_pool = mock_pool

        result = handler.get_session_instance("session-123")

        mock_pool.get_session_instance.assert_called_once_with("session-123")
        self.assertIsNotNone(result)
        self.assertEqual(result.session_id, "session-123")

    @patch("clud.api.instance_manager.InstancePool")
    def test_get_all_instances(self, mock_pool_class: MagicMock) -> None:
        """Test getting all instances."""
        # Create mock instances
        mock_instances = [
            MagicMock(instance_id="instance-1"),
            MagicMock(instance_id="instance-2"),
        ]

        # Setup mock pool
        mock_pool = MagicMock()
        mock_pool.get_all_instances.return_value = mock_instances
        mock_pool_class.return_value = mock_pool

        handler = MessageHandler()
        handler._instance_pool = mock_pool

        result = handler.get_all_instances()

        self.assertEqual(len(result), 2)
        self.assertEqual(result[0].instance_id, "instance-1")
        self.assertEqual(result[1].instance_id, "instance-2")

    @patch("clud.api.instance_manager.InstancePool")
    def test_delete_instance(self, mock_pool_class: MagicMock) -> None:
        """Test deleting an instance."""

        async def run_test() -> None:
            # Setup mock pool
            mock_pool = MagicMock()
            mock_pool.delete_instance = AsyncMock(return_value=True)
            mock_pool_class.return_value = mock_pool

            handler = MessageHandler()
            handler._instance_pool = mock_pool

            result = await handler.delete_instance("instance-123")

            mock_pool.delete_instance.assert_called_once_with("instance-123")
            self.assertTrue(result)

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool")
    def test_cleanup_idle_instances(self, mock_pool_class: MagicMock) -> None:
        """Test cleaning up idle instances."""

        async def run_test() -> None:
            # Setup mock pool
            mock_pool = MagicMock()
            mock_pool.cleanup_idle_instances = AsyncMock(return_value=3)
            mock_pool_class.return_value = mock_pool

            handler = MessageHandler()
            handler._instance_pool = mock_pool

            count = await handler.cleanup_idle_instances(1800)

            mock_pool.cleanup_idle_instances.assert_called_once()
            self.assertEqual(count, 3)

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool")
    def test_start_cleanup_task(self, mock_pool_class: MagicMock) -> None:
        """Test starting the cleanup task."""

        async def run_test() -> None:
            # Setup mock pool
            mock_pool = MagicMock()
            mock_pool.start_cleanup_task = AsyncMock()
            mock_pool_class.return_value = mock_pool

            handler = MessageHandler()
            handler._instance_pool = mock_pool

            await handler.start_cleanup_task(300)

            mock_pool.start_cleanup_task.assert_called_once_with(300)

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.InstancePool")
    def test_shutdown(self, mock_pool_class: MagicMock) -> None:
        """Test shutting down the message handler."""

        async def run_test() -> None:
            # Setup mock pool
            mock_pool = MagicMock()
            mock_pool.shutdown = AsyncMock()
            mock_pool_class.return_value = mock_pool

            handler = MessageHandler()
            handler._instance_pool = mock_pool

            await handler.shutdown()

            mock_pool.shutdown.assert_called_once()

        asyncio.run(run_test())


if __name__ == "__main__":
    unittest.main()
