"""SMS messaging implementation for Claude agents."""

import logging
from typing import Optional

logger = logging.getLogger(__name__)


class SMSMessenger:
    """SMS messenger for agent notifications using Twilio."""

    def __init__(self, account_sid: str, auth_token: str, from_number: str, to_number: str):
        """Initialize SMS messenger.

        Args:
            account_sid: Twilio account SID
            auth_token: Twilio auth token
            from_number: Phone number to send from
            to_number: Phone number to send to
        """
        self.account_sid = account_sid
        self.auth_token = auth_token
        self.from_number = from_number
        self.to_number = to_number
        self.client = None
        self._initialized = False

    def _ensure_initialized(self) -> bool:
        """Ensure Twilio client is initialized."""
        if self._initialized:
            return True

        try:
            # Import here to avoid requiring twilio library if not used
            from twilio.rest import Client

            self.client = Client(self.account_sid, self.auth_token)
            self._initialized = True
            logger.info("SMS messenger initialized successfully")
            return True
        except ImportError:
            logger.error("twilio not installed. Install with: pip install twilio")
            return False
        except Exception as e:
            logger.error(f"Failed to initialize SMS messenger: {e}")
            return False

    async def send_invitation(self, agent_name: str, container_id: str, metadata: dict) -> bool:
        """Send invitation message when agent launches.

        Args:
            agent_name: Name of the agent
            container_id: Docker container ID
            metadata: Additional metadata about the agent

        Returns:
            True if message sent successfully, False otherwise
        """
        if not self._ensure_initialized():
            return False

        try:
            message = f"🤖 Claude Agent '{agent_name}' is online! Container: {container_id[:8]}. Reply to interact."

            self.client.messages.create(body=message, from_=self.from_number, to=self.to_number)
            logger.info(f"Sent SMS invitation for agent {agent_name}")
            return True
        except Exception as e:
            logger.error(f"Failed to send SMS invitation: {e}")
            return False

    async def send_status_update(self, agent_name: str, status: str, details: Optional[dict] = None) -> bool:
        """Send status update during agent operation.

        Args:
            agent_name: Name of the agent
            status: Current status message
            details: Optional additional details

        Returns:
            True if message sent successfully, False otherwise
        """
        if not self._ensure_initialized():
            return False

        try:
            message = f"📊 Agent '{agent_name}': {status}"
            if details:
                # Add key details (limited by SMS length)
                for key, value in list(details.items())[:2]:
                    message += f" {key}={value}"

            self.client.messages.create(body=message, from_=self.from_number, to=self.to_number)
            logger.info(f"Sent SMS status update for agent {agent_name}")
            return True
        except Exception as e:
            logger.error(f"Failed to send SMS status: {e}")
            return False

    async def send_cleanup_notification(self, agent_name: str, summary: dict) -> bool:
        """Send notification when agent cleans up.

        Args:
            agent_name: Name of the agent
            summary: Summary of agent execution (duration, tasks, etc.)

        Returns:
            True if message sent successfully, False otherwise
        """
        if not self._ensure_initialized():
            return False

        try:
            message = f"✅ Agent '{agent_name}' completed. Tasks: {summary.get('tasks_completed', 0)}, Duration: {summary.get('duration', 'N/A')}"

            self.client.messages.create(body=message, from_=self.from_number, to=self.to_number)
            logger.info(f"Sent SMS cleanup notification for agent {agent_name}")
            return True
        except Exception as e:
            logger.error(f"Failed to send SMS cleanup: {e}")
            return False

    async def receive_message(self, timeout: int = 60) -> Optional[str]:
        """Receive message from user.

        Note: SMS receiving requires webhook setup in Twilio console.
        This is a placeholder for webhook-based message reception.

        Args:
            timeout: Timeout in seconds

        Returns:
            Message text if received, None otherwise
        """
        logger.warning("SMS message reception requires webhook configuration in Twilio console")
        return None
