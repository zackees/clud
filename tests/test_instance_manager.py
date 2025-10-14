"""Unit tests for instance manager."""

import asyncio
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

from clud.api.instance_manager import CludInstance, InstancePool
from clud.api.models import ClientType, ExecutionStatus


class TestCludInstance(unittest.TestCase):
    """Tests for CludInstance class."""

    def test_clud_instance_creation(self) -> None:
        """Test creating a CludInstance."""
        instance = CludInstance(
            instance_id="test-instance",
            session_id="test-session",
            client_type=ClientType.API,
            client_id="test-client",
        )

        self.assertEqual(instance.instance_id, "test-instance")
        self.assertEqual(instance.session_id, "test-session")
        self.assertEqual(instance.client_type, ClientType.API)
        self.assertEqual(instance.client_id, "test-client")
        self.assertIsNone(instance.process)
        self.assertEqual(instance.status, ExecutionStatus.PENDING)
        self.assertEqual(instance.message_count, 0)
        self.assertEqual(instance.output_buffer, [])

    def test_clud_instance_with_working_directory(self) -> None:
        """Test creating a CludInstance with working directory."""
        instance = CludInstance(
            instance_id="test-instance",
            session_id="test-session",
            client_type=ClientType.API,
            client_id="test-client",
            working_directory=Path("/home/user/project"),
        )

        self.assertEqual(instance.working_directory, Path("/home/user/project"))

    def test_clud_instance_start(self) -> None:
        """Test starting a CludInstance."""

        async def run_test() -> None:
            instance = CludInstance(
                instance_id="test-instance",
                session_id="test-session",
                client_type=ClientType.API,
                client_id="test-client",
            )

            await instance.start()

            self.assertEqual(instance.status, ExecutionStatus.RUNNING)

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.asyncio.create_subprocess_exec")
    def test_clud_instance_execute(self, mock_create_subprocess: MagicMock) -> None:
        """Test executing a command in a CludInstance."""

        async def run_test() -> None:
            # Create mock process
            mock_process = MagicMock()
            mock_process.communicate = AsyncMock(return_value=(b"test output\n", b""))
            mock_process.returncode = 0

            mock_create_subprocess.return_value = mock_process

            instance = CludInstance(
                instance_id="test-instance",
                session_id="test-session",
                client_type=ClientType.API,
                client_id="test-client",
            )

            await instance.start()
            result = await instance.execute("echo test")

            # Verify execution result
            self.assertEqual(result["status"], "completed")
            self.assertEqual(result["output"], "test output\n")
            self.assertIsNone(result["error"])
            self.assertEqual(result["exit_code"], 0)

            # Verify instance state
            self.assertEqual(instance.status, ExecutionStatus.COMPLETED)
            self.assertEqual(instance.message_count, 1)
            self.assertIsNone(instance.process)  # Process cleared after execution

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.asyncio.create_subprocess_exec")
    def test_clud_instance_execute_with_error(self, mock_create_subprocess: MagicMock) -> None:
        """Test executing a command that fails."""

        async def run_test() -> None:
            # Create mock process that fails
            mock_process = MagicMock()
            mock_process.communicate = AsyncMock(return_value=(b"", b"error message\n"))
            mock_process.returncode = 1

            mock_create_subprocess.return_value = mock_process

            instance = CludInstance(
                instance_id="test-instance",
                session_id="test-session",
                client_type=ClientType.API,
                client_id="test-client",
            )

            await instance.start()
            result = await instance.execute("failing command")

            # Verify execution result
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["error"], "error message\n")
            self.assertEqual(result["exit_code"], 1)

            # Verify instance state
            self.assertEqual(instance.status, ExecutionStatus.FAILED)

        asyncio.run(run_test())

    @patch("clud.api.instance_manager.asyncio.create_subprocess_exec")
    def test_clud_instance_execute_with_working_directory(self, mock_create_subprocess: MagicMock) -> None:
        """Test executing a command with a working directory."""

        async def run_test() -> None:
            # Create mock process
            mock_process = MagicMock()
            mock_process.communicate = AsyncMock(return_value=(b"output\n", b""))
            mock_process.returncode = 0

            mock_create_subprocess.return_value = mock_process

            instance = CludInstance(
                instance_id="test-instance",
                session_id="test-session",
                client_type=ClientType.API,
                client_id="test-client",
                working_directory=Path("/home/user/project"),
            )

            await instance.start()
            await instance.execute("ls")

            # Verify subprocess was called with cwd
            args, kwargs = mock_create_subprocess.call_args
            self.assertEqual(kwargs.get("cwd"), str(Path("/home/user/project")))

        asyncio.run(run_test())

    def test_clud_instance_stop(self) -> None:
        """Test stopping a CludInstance."""

        async def run_test() -> None:
            instance = CludInstance(
                instance_id="test-instance",
                session_id="test-session",
                client_type=ClientType.API,
                client_id="test-client",
            )

            await instance.start()
            await instance.stop()

            self.assertEqual(instance.status, ExecutionStatus.COMPLETED)

        asyncio.run(run_test())

    def test_clud_instance_to_instance_info(self) -> None:
        """Test converting CludInstance to InstanceInfo."""
        instance = CludInstance(
            instance_id="test-instance",
            session_id="test-session",
            client_type=ClientType.TELEGRAM,
            client_id="test-client",
            working_directory=Path("/home/user/project"),
        )
        instance.message_count = 5
        instance.status = ExecutionStatus.RUNNING

        info = instance.to_instance_info()

        self.assertEqual(info.instance_id, "test-instance")
        self.assertEqual(info.session_id, "test-session")
        self.assertEqual(info.client_type, ClientType.TELEGRAM)
        self.assertEqual(info.client_id, "test-client")
        self.assertEqual(info.working_directory, str(Path("/home/user/project")))
        self.assertEqual(info.message_count, 5)
        self.assertEqual(info.status, ExecutionStatus.RUNNING)


class TestInstancePool(unittest.TestCase):
    """Tests for InstancePool class."""

    def setUp(self) -> None:
        """Set up test fixtures."""
        self.pool = InstancePool(max_instances=5, idle_timeout_seconds=60)

    def tearDown(self) -> None:
        """Clean up after tests."""
        asyncio.run(self.pool.shutdown())

    def test_instance_pool_initialization(self) -> None:
        """Test InstancePool initialization."""
        pool = InstancePool(max_instances=10, idle_timeout_seconds=300)

        self.assertEqual(pool.max_instances, 10)
        self.assertEqual(pool.idle_timeout_seconds, 300)
        self.assertEqual(len(pool.instances_by_id), 0)
        self.assertEqual(len(pool.instances_by_session), 0)

    def test_get_or_create_instance_creates_new(self) -> None:
        """Test that get_or_create_instance creates a new instance."""

        async def run_test() -> None:
            instance = await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            self.assertIsNotNone(instance)
            self.assertEqual(instance.session_id, "session-1")
            self.assertEqual(instance.client_type, ClientType.API)
            self.assertEqual(len(self.pool.instances_by_id), 1)
            self.assertEqual(len(self.pool.instances_by_session), 1)

        asyncio.run(run_test())

    def test_get_or_create_instance_reuses_existing(self) -> None:
        """Test that get_or_create_instance reuses existing instance for same session."""

        async def run_test() -> None:
            # Create first instance
            instance1 = await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            # Request again with same session_id
            instance2 = await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            # Should be the same instance
            self.assertIs(instance1, instance2)
            self.assertEqual(len(self.pool.instances_by_id), 1)

        asyncio.run(run_test())

    def test_get_or_create_instance_max_instances_limit(self) -> None:
        """Test that max instances limit is enforced."""

        async def run_test() -> None:
            # Create max instances
            for i in range(self.pool.max_instances):
                await self.pool.get_or_create_instance(
                    session_id=f"session-{i}",
                    client_type=ClientType.API,
                    client_id=f"client-{i}",
                )

            # Try to create one more - should raise
            with self.assertRaises(RuntimeError) as context:
                await self.pool.get_or_create_instance(
                    session_id="session-overflow",
                    client_type=ClientType.API,
                    client_id="client-overflow",
                )

            self.assertIn("Maximum instance limit", str(context.exception))

        asyncio.run(run_test())

    def test_get_instance(self) -> None:
        """Test getting an instance by ID."""

        async def run_test() -> None:
            # Create instance
            instance = await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            # Get by ID
            retrieved = self.pool.get_instance(instance.instance_id)

            self.assertIs(retrieved, instance)

        asyncio.run(run_test())

    def test_get_instance_not_found(self) -> None:
        """Test getting a non-existent instance."""
        result = self.pool.get_instance("nonexistent-id")
        self.assertIsNone(result)

    def test_get_session_instance(self) -> None:
        """Test getting an instance by session ID."""

        async def run_test() -> None:
            # Create instance
            instance = await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            # Get by session ID
            retrieved = self.pool.get_session_instance("session-1")

            self.assertIs(retrieved, instance)

        asyncio.run(run_test())

    def test_get_all_instances(self) -> None:
        """Test getting all instances."""

        async def run_test() -> None:
            # Create multiple instances
            await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )
            await self.pool.get_or_create_instance(
                session_id="session-2",
                client_type=ClientType.TELEGRAM,
                client_id="client-2",
            )

            # Get all instances
            all_instances = self.pool.get_all_instances()

            self.assertEqual(len(all_instances), 2)
            session_ids = [info.session_id for info in all_instances]
            self.assertIn("session-1", session_ids)
            self.assertIn("session-2", session_ids)

        asyncio.run(run_test())

    def test_delete_instance(self) -> None:
        """Test deleting an instance."""

        async def run_test() -> None:
            # Create instance
            instance = await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            # Delete it
            result = await self.pool.delete_instance(instance.instance_id)

            self.assertTrue(result)
            self.assertEqual(len(self.pool.instances_by_id), 0)
            self.assertEqual(len(self.pool.instances_by_session), 0)

        asyncio.run(run_test())

    def test_delete_nonexistent_instance(self) -> None:
        """Test deleting a non-existent instance."""

        async def run_test() -> None:
            result = await self.pool.delete_instance("nonexistent-id")
            self.assertFalse(result)

        asyncio.run(run_test())

    def test_cleanup_idle_instances(self) -> None:
        """Test cleaning up idle instances."""

        async def run_test() -> None:
            # Create instance
            instance = await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            # Manually set last_activity to old time
            instance.last_activity = datetime.now() - timedelta(seconds=self.pool.idle_timeout_seconds + 10)

            # Run cleanup
            count = await self.pool.cleanup_idle_instances()

            # Should have cleaned up 1 instance
            self.assertEqual(count, 1)
            self.assertEqual(len(self.pool.instances_by_id), 0)

        asyncio.run(run_test())

    def test_cleanup_idle_instances_keeps_active(self) -> None:
        """Test that cleanup doesn't remove active instances."""

        async def run_test() -> None:
            # Create instance
            await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )

            # Run cleanup immediately
            count = await self.pool.cleanup_idle_instances()

            # Should not have cleaned up anything
            self.assertEqual(count, 0)
            self.assertEqual(len(self.pool.instances_by_id), 1)

        asyncio.run(run_test())

    def test_start_cleanup_task(self) -> None:
        """Test starting the cleanup task."""

        async def run_test() -> None:
            await self.pool.start_cleanup_task(interval_seconds=1)

            self.assertIsNotNone(self.pool._cleanup_task)

            # Stop it immediately
            await self.pool.stop_cleanup_task()

        asyncio.run(run_test())

    def test_stop_cleanup_task(self) -> None:
        """Test stopping the cleanup task."""

        async def run_test() -> None:
            await self.pool.start_cleanup_task(interval_seconds=1)
            await self.pool.stop_cleanup_task()

            self.assertIsNone(self.pool._cleanup_task)

        asyncio.run(run_test())

    def test_shutdown(self) -> None:
        """Test shutting down the pool."""

        async def run_test() -> None:
            # Create some instances
            await self.pool.get_or_create_instance(
                session_id="session-1",
                client_type=ClientType.API,
                client_id="client-1",
            )
            await self.pool.get_or_create_instance(
                session_id="session-2",
                client_type=ClientType.API,
                client_id="client-2",
            )

            # Shutdown
            await self.pool.shutdown()

            # All instances should be cleared
            self.assertEqual(len(self.pool.instances_by_id), 0)
            self.assertEqual(len(self.pool.instances_by_session), 0)

        asyncio.run(run_test())


if __name__ == "__main__":
    unittest.main()
