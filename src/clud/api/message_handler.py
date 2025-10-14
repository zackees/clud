"""Message handler for routing messages to clud instances."""

import logging
from pathlib import Path

from clud.api.instance_manager import InstancePool
from clud.api.models import (
    ExecutionStatus,
    InstanceInfo,
    MessageRequest,
    MessageResponse,
)

logger = logging.getLogger(__name__)


class MessageHandler:
    """Handles message routing to clud instances."""

    def __init__(self, max_instances: int = 100, idle_timeout_seconds: int = 1800) -> None:
        """Initialize the message handler.

        Args:
            max_instances: Maximum number of concurrent instances
            idle_timeout_seconds: Seconds before idle instances are cleaned up
        """
        self._instance_pool = InstancePool(max_instances=max_instances, idle_timeout_seconds=idle_timeout_seconds)
        logger.info(f"MessageHandler initialized with max_instances={max_instances}, idle_timeout={idle_timeout_seconds}s")

    async def handle_message(self, request: MessageRequest) -> MessageResponse:
        """
        Handle an incoming message from a client.

        Args:
            request: The message request to handle

        Returns:
            MessageResponse with instance ID and status
        """
        # Validate request
        is_valid, error_msg = request.validate()
        if not is_valid:
            logger.warning(f"Invalid message request: {error_msg}")
            return MessageResponse(
                instance_id="",
                session_id=request.session_id,
                status=ExecutionStatus.FAILED,
                error=error_msg,
            )

        try:
            # Convert working_directory to Path if provided
            working_dir = Path(request.working_directory) if request.working_directory else None

            # Get or create instance for this session
            instance = await self._instance_pool.get_or_create_instance(
                session_id=request.session_id,
                client_type=request.client_type,
                client_id=request.client_id,
                working_directory=working_dir,
            )

            # Register hooks based on client type (placeholder for now)
            await self._register_hooks(instance.to_instance_info(), request)

            logger.info(f"Handling message for session {request.session_id}, instance {instance.instance_id}, message count {instance.message_count}")

            # Execute the message
            result = await instance.execute(request.message)

            # Determine status from result
            if result["status"] == "completed":
                status = ExecutionStatus.COMPLETED
            elif result["status"] == "failed":
                status = ExecutionStatus.FAILED
            else:
                status = ExecutionStatus.RUNNING

            # Return response with execution result
            return MessageResponse(
                instance_id=instance.instance_id,
                session_id=request.session_id,
                status=status,
                message=result.get("output", ""),
                error=result.get("error"),
                metadata={
                    "message_count": instance.message_count,
                    "client_type": request.client_type.value,
                    "exit_code": result.get("exit_code"),
                },
            )

        except Exception as e:
            logger.exception(f"Error handling message: {e}")
            return MessageResponse(
                instance_id="",
                session_id=request.session_id,
                status=ExecutionStatus.FAILED,
                error=f"Internal error: {str(e)}",
            )

    async def _register_hooks(self, instance: InstanceInfo, request: MessageRequest) -> None:
        """
        Register hooks for this instance based on client type.

        Args:
            instance: The instance to register hooks for
            request: The message request
        """
        # TODO: Implement hook registration when hooks module is available
        # For now, this is a placeholder
        logger.debug(f"Hook registration for instance {instance.instance_id}, client_type={request.client_type.value} (not implemented yet)")

    def get_instance(self, instance_id: str) -> InstanceInfo | None:
        """
        Get instance by ID.

        Args:
            instance_id: The instance ID

        Returns:
            InstanceInfo or None if not found
        """
        instance = self._instance_pool.get_instance(instance_id)
        return instance.to_instance_info() if instance else None

    def get_session_instance(self, session_id: str) -> InstanceInfo | None:
        """
        Get instance by session ID.

        Args:
            session_id: The session ID

        Returns:
            InstanceInfo or None if not found
        """
        instance = self._instance_pool.get_session_instance(session_id)
        return instance.to_instance_info() if instance else None

    def get_all_instances(self) -> list[InstanceInfo]:
        """
        Get all active instances.

        Returns:
            List of all InstanceInfo objects
        """
        return self._instance_pool.get_all_instances()

    async def delete_instance(self, instance_id: str) -> bool:
        """
        Delete an instance and its session.

        Args:
            instance_id: The instance ID to delete

        Returns:
            True if deleted, False if not found
        """
        return await self._instance_pool.delete_instance(instance_id)

    async def cleanup_idle_instances(self, max_idle_seconds: int = 1800) -> int:
        """
        Clean up instances that have been idle for too long.

        Args:
            max_idle_seconds: Maximum idle time in seconds (default: 30 minutes)

        Returns:
            Number of instances cleaned up
        """
        return await self._instance_pool.cleanup_idle_instances()

    async def start_cleanup_task(self, interval_seconds: int = 300) -> None:
        """
        Start background task to periodically clean up idle instances.

        Args:
            interval_seconds: Interval between cleanup runs (default 5 minutes)
        """
        await self._instance_pool.start_cleanup_task(interval_seconds)

    async def shutdown(self) -> None:
        """Shut down the message handler and all instances."""
        await self._instance_pool.shutdown()
