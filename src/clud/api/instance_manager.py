"""Instance manager for clud subprocess lifecycle management.

This module provides classes for managing clud subprocess instances, including:
- CludInstance: Individual clud process wrapper with output streaming
- InstancePool: Pool of instances with lifecycle management and cleanup
"""

import asyncio
import contextlib
import logging
import sys
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

from clud.api.models import ClientType, ExecutionStatus, InstanceInfo

logger = logging.getLogger(__name__)


@dataclass
class CludInstance:
    """Manages a single clud subprocess instance.

    Provides methods for starting, executing commands, stopping, and streaming
    output from a clud process.
    """

    instance_id: str
    session_id: str
    client_type: ClientType
    client_id: str
    working_directory: Path | None = None
    process: asyncio.subprocess.Process | None = None
    status: ExecutionStatus = ExecutionStatus.PENDING
    created_at: datetime = field(default_factory=datetime.now)
    last_activity: datetime = field(default_factory=datetime.now)
    message_count: int = 0
    output_buffer: list[str] = field(default_factory=lambda: [])
    metadata: dict[str, Any] = field(default_factory=lambda: {})

    def __post_init__(self) -> None:
        """Initialize instance with default values."""
        if self.working_directory and isinstance(self.working_directory, str):
            self.working_directory = Path(self.working_directory)

    async def start(self) -> None:
        """Start the clud subprocess.

        Launches a clud process in YOLO mode (--dangerously-skip-permissions)
        that will wait for messages to execute.

        Raises:
            RuntimeError: If process fails to start
        """
        try:
            logger.info(f"Starting clud instance {self.instance_id} for session {self.session_id}")

            # For now, we don't start a persistent process
            # Instead, we'll spawn processes on-demand for each execute() call
            # This is simpler and more reliable than maintaining a persistent shell
            self.status = ExecutionStatus.RUNNING
            self.last_activity = datetime.now()

            logger.info(f"Instance {self.instance_id} initialized and ready")

        except Exception as e:
            logger.error(f"Failed to start instance {self.instance_id}: {e}")
            self.status = ExecutionStatus.FAILED
            raise RuntimeError(f"Failed to start clud instance: {e}") from e

    async def execute(self, message: str) -> dict[str, Any]:
        """Execute a command/message in this instance.

        Args:
            message: The message/prompt to send to clud

        Returns:
            dict containing execution result with keys: status, output, error, exit_code

        Raises:
            RuntimeError: If execution fails
        """
        try:
            logger.info(f"Executing message in instance {self.instance_id}: {message[:50]}...")

            self.message_count += 1
            self.last_activity = datetime.now()
            self.status = ExecutionStatus.RUNNING

            # Build command
            cmd = [sys.executable, "-m", "clud.agent_cli", "--dangerously-skip-permissions", "-p", message]

            # Add working directory if specified
            cwd = str(self.working_directory) if self.working_directory else None

            # Execute command
            process = await asyncio.create_subprocess_exec(
                *cmd,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                cwd=cwd,
            )

            self.process = process

            # Read output asynchronously
            stdout_data, stderr_data = await process.communicate()

            stdout = stdout_data.decode("utf-8", errors="replace") if stdout_data else ""
            stderr = stderr_data.decode("utf-8", errors="replace") if stderr_data else ""

            # Store output in buffer
            if stdout:
                self.output_buffer.append(stdout)
            if stderr:
                self.output_buffer.append(f"STDERR: {stderr}")

            # Check exit code
            exit_code = process.returncode

            if exit_code == 0:
                self.status = ExecutionStatus.COMPLETED
                logger.info(f"Instance {self.instance_id} completed execution successfully")
                return {
                    "status": "completed",
                    "output": stdout,
                    "error": stderr if stderr else None,
                    "exit_code": exit_code,
                }
            else:
                self.status = ExecutionStatus.FAILED
                logger.error(f"Instance {self.instance_id} execution failed with exit code {exit_code}")
                return {
                    "status": "failed",
                    "output": stdout,
                    "error": stderr,
                    "exit_code": exit_code,
                }

        except Exception as e:
            logger.error(f"Failed to execute command in instance {self.instance_id}: {e}")
            self.status = ExecutionStatus.FAILED
            return {
                "status": "failed",
                "output": "",
                "error": str(e),
                "exit_code": -1,
            }
        finally:
            self.process = None

    async def stop(self) -> None:
        """Stop the clud subprocess gracefully.

        Attempts graceful shutdown first, then forceful termination if needed.
        """
        try:
            logger.info(f"Stopping instance {self.instance_id}")

            if self.process and self.process.returncode is None:
                # Try graceful shutdown
                try:
                    self.process.terminate()
                    await asyncio.wait_for(self.process.wait(), timeout=5.0)
                except asyncio.TimeoutError:
                    # Force kill if graceful shutdown times out
                    logger.warning(f"Instance {self.instance_id} did not terminate gracefully, killing")
                    self.process.kill()
                    await self.process.wait()

            self.status = ExecutionStatus.COMPLETED
            logger.info(f"Instance {self.instance_id} stopped successfully")

        except Exception as e:
            logger.error(f"Error stopping instance {self.instance_id}: {e}")

    def to_instance_info(self) -> InstanceInfo:
        """Convert to InstanceInfo dataclass.

        Returns:
            InstanceInfo containing current instance state
        """
        return InstanceInfo(
            instance_id=self.instance_id,
            session_id=self.session_id,
            client_type=self.client_type,
            client_id=self.client_id,
            working_directory=str(self.working_directory) if self.working_directory else None,
            status=self.status,
            created_at=self.created_at,
            last_activity=self.last_activity,
            message_count=self.message_count,
            metadata=self.metadata,
        )


class InstancePool:
    """Manages a pool of CludInstance objects.

    Provides instance lifecycle management, reuse, cleanup, and metrics tracking.
    """

    def __init__(self, max_instances: int = 100, idle_timeout_seconds: int = 1800) -> None:
        """Initialize instance pool.

        Args:
            max_instances: Maximum number of concurrent instances allowed
            idle_timeout_seconds: Seconds of inactivity before instance cleanup (default 30 min)
        """
        self.max_instances = max_instances
        self.idle_timeout_seconds = idle_timeout_seconds
        self.instances_by_id: dict[str, CludInstance] = {}
        self.instances_by_session: dict[str, CludInstance] = {}
        self._cleanup_task: asyncio.Task[None] | None = None
        logger.info(f"InstancePool initialized: max_instances={max_instances}, idle_timeout={idle_timeout_seconds}s")

    async def get_or_create_instance(
        self,
        session_id: str,
        client_type: ClientType,
        client_id: str,
        working_directory: Path | None = None,
    ) -> CludInstance:
        """Get existing instance for session or create new one.

        Args:
            session_id: Session identifier
            client_type: Type of client (API, TELEGRAM, WEB, etc.)
            client_id: Client identifier
            working_directory: Working directory for clud process

        Returns:
            CludInstance (existing or newly created)

        Raises:
            RuntimeError: If max instances limit reached
        """
        # Check if instance exists for this session
        if session_id in self.instances_by_session:
            instance = self.instances_by_session[session_id]
            instance.last_activity = datetime.now()
            logger.info(f"Reusing existing instance {instance.instance_id} for session {session_id}")
            return instance

        # Check max instances limit
        if len(self.instances_by_id) >= self.max_instances:
            logger.error(f"Max instances limit reached: {self.max_instances}")
            raise RuntimeError(f"Maximum instance limit reached ({self.max_instances})")

        # Create new instance
        instance_id = str(uuid.uuid4())
        instance = CludInstance(
            instance_id=instance_id,
            session_id=session_id,
            client_type=client_type,
            client_id=client_id,
            working_directory=working_directory,
        )

        # Start the instance
        await instance.start()

        # Store in both dictionaries
        self.instances_by_id[instance_id] = instance
        self.instances_by_session[session_id] = instance

        logger.info(f"Created new instance {instance_id} for session {session_id}")
        return instance

    def get_instance(self, instance_id: str) -> CludInstance | None:
        """Get instance by ID.

        Args:
            instance_id: Instance identifier

        Returns:
            CludInstance or None if not found
        """
        return self.instances_by_id.get(instance_id)

    def get_session_instance(self, session_id: str) -> CludInstance | None:
        """Get instance by session ID.

        Args:
            session_id: Session identifier

        Returns:
            CludInstance or None if not found
        """
        return self.instances_by_session.get(session_id)

    def get_all_instances(self) -> list[InstanceInfo]:
        """Get information about all active instances.

        Returns:
            List of InstanceInfo objects
        """
        return [instance.to_instance_info() for instance in self.instances_by_id.values()]

    async def delete_instance(self, instance_id: str) -> bool:
        """Delete an instance and clean up resources.

        Args:
            instance_id: Instance identifier

        Returns:
            True if instance was deleted, False if not found
        """
        instance = self.instances_by_id.get(instance_id)
        if not instance:
            logger.warning(f"Instance {instance_id} not found for deletion")
            return False

        # Stop the instance
        await instance.stop()

        # Remove from both dictionaries
        del self.instances_by_id[instance_id]
        if instance.session_id in self.instances_by_session:
            del self.instances_by_session[instance.session_id]

        logger.info(f"Deleted instance {instance_id}")
        return True

    async def cleanup_idle_instances(self) -> int:
        """Clean up instances that have been idle for too long.

        Returns:
            Number of instances cleaned up
        """
        now = datetime.now()
        cleanup_count = 0

        # Find idle instances
        instances_to_delete: list[str] = []
        for instance in self.instances_by_id.values():
            idle_seconds = (now - instance.last_activity).total_seconds()
            if idle_seconds > self.idle_timeout_seconds:
                instances_to_delete.append(instance.instance_id)
                logger.info(f"Instance {instance.instance_id} idle for {idle_seconds:.0f}s, cleaning up")

        # Delete idle instances
        for instance_id in instances_to_delete:
            if await self.delete_instance(instance_id):
                cleanup_count += 1

        if cleanup_count > 0:
            logger.info(f"Cleaned up {cleanup_count} idle instances")

        return cleanup_count

    async def start_cleanup_task(self, interval_seconds: int = 300) -> None:
        """Start background task to periodically clean up idle instances.

        Args:
            interval_seconds: Interval between cleanup runs (default 5 minutes)
        """
        if self._cleanup_task is not None:
            logger.warning("Cleanup task already running")
            return

        async def cleanup_loop() -> None:
            """Background cleanup loop."""
            while True:
                try:
                    await asyncio.sleep(interval_seconds)
                    await self.cleanup_idle_instances()
                except asyncio.CancelledError:
                    logger.info("Cleanup task cancelled")
                    break
                except Exception as e:
                    logger.error(f"Error in cleanup task: {e}")

        self._cleanup_task = asyncio.create_task(cleanup_loop())
        logger.info(f"Started cleanup task with interval {interval_seconds}s")

    async def stop_cleanup_task(self) -> None:
        """Stop the background cleanup task."""
        if self._cleanup_task is not None:
            self._cleanup_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._cleanup_task
            self._cleanup_task = None
            logger.info("Stopped cleanup task")

    async def shutdown(self) -> None:
        """Shut down all instances and cleanup resources."""
        logger.info(f"Shutting down instance pool with {len(self.instances_by_id)} instances")

        # Stop cleanup task
        await self.stop_cleanup_task()

        # Stop all instances
        for instance in list(self.instances_by_id.values()):
            await instance.stop()

        # Clear dictionaries
        self.instances_by_id.clear()
        self.instances_by_session.clear()

        logger.info("Instance pool shutdown complete")
