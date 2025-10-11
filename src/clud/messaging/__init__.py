"""Messaging module for Claude agent notifications via Telegram."""

from typing import Optional, Protocol


class AgentMessenger(Protocol):
    """Protocol for agent messaging implementations."""

    async def send_invitation(self, agent_name: str, container_id: str, metadata: dict) -> bool:
        """Send invitation message when agent launches.

        Args:
            agent_name: Name of the agent
            container_id: Docker container ID
            metadata: Additional metadata about the agent

        Returns:
            True if message sent successfully, False otherwise
        """
        ...

    async def send_status_update(self, agent_name: str, status: str, details: Optional[dict] = None) -> bool:
        """Send status update during agent operation.

        Args:
            agent_name: Name of the agent
            status: Current status message
            details: Optional additional details

        Returns:
            True if message sent successfully, False otherwise
        """
        ...

    async def send_cleanup_notification(self, agent_name: str, summary: dict) -> bool:
        """Send notification when agent cleans up.

        Args:
            agent_name: Name of the agent
            summary: Summary of agent execution (duration, tasks, etc.)

        Returns:
            True if message sent successfully, False otherwise
        """
        ...

    async def receive_message(self, timeout: int = 60) -> Optional[str]:
        """Receive message from user.

        Args:
            timeout: Timeout in seconds

        Returns:
            Message text if received, None otherwise
        """
        ...


from .telegram import TelegramMessenger

__all__ = [
    "AgentMessenger",
    "TelegramMessenger",
]
