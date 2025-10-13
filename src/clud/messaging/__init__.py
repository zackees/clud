"""Messaging module for Claude agent notifications via Telegram."""

from typing import Protocol

from .telegram import TelegramMessenger


class AgentMessenger(Protocol):
    """Protocol for agent messaging implementations."""

    async def send_invitation(self, agent_name: str, process_id: str, metadata: dict[str, str]) -> bool:
        """Send invitation message when agent launches.

        Args:
            agent_name: Name of the agent
            process_id: Process ID
            metadata: Additional metadata about the agent

        Returns:
            True if message sent successfully, False otherwise
        """
        ...

    async def send_status_update(self, agent_name: str, status: str, details: dict[str, str] | None = None) -> bool:
        """Send status update during agent operation.

        Args:
            agent_name: Name of the agent
            status: Current status message
            details: Optional additional details

        Returns:
            True if message sent successfully, False otherwise
        """
        ...

    async def send_cleanup_notification(self, agent_name: str, summary: dict[str, int | str]) -> bool:
        """Send notification when agent cleans up.

        Args:
            agent_name: Name of the agent
            summary: Summary of agent execution (duration, tasks, etc.)

        Returns:
            True if message sent successfully, False otherwise
        """
        ...

    async def receive_message(self, timeout: int = 60) -> str | None:
        """Receive message from user.

        Args:
            timeout: Timeout in seconds

        Returns:
            Message text if received, None otherwise
        """
        ...


__all__ = [
    "AgentMessenger",
    "TelegramMessenger",
]
