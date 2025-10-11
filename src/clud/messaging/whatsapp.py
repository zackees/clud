"""WhatsApp messaging implementation for Claude agents."""

import logging
from typing import Optional

logger = logging.getLogger(__name__)


class WhatsAppMessenger:
    """WhatsApp messenger for agent notifications using Meta Cloud API."""

    def __init__(self, phone_number_id: str, access_token: str, to_number: str):
        """Initialize WhatsApp messenger.

        Args:
            phone_number_id: WhatsApp phone number ID
            access_token: Meta access token
            to_number: Phone number to send to (in international format)
        """
        self.phone_number_id = phone_number_id
        self.access_token = access_token
        self.to_number = to_number
        self.base_url = "https://graph.facebook.com/v18.0"
        self._initialized = False

    def _ensure_initialized(self) -> bool:
        """Ensure messenger is ready."""
        if self._initialized:
            return True

        try:
            # Import here to avoid requiring requests library if not used
            import requests  # noqa: F401

            self._initialized = True
            logger.info("WhatsApp messenger initialized successfully")
            return True
        except ImportError:
            logger.error("requests not installed. Install with: pip install requests")
            return False
        except Exception as e:
            logger.error(f"Failed to initialize WhatsApp messenger: {e}")
            return False

    async def send_invitation(self, agent_name: str, container_id: str, metadata: dict) -> bool:
        """Send invitation message when agent launches.

        Note: WhatsApp requires pre-approved templates for proactive messages.
        This uses a text message which only works within 24h of user message.

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
            import requests

            url = f"{self.base_url}/{self.phone_number_id}/messages"
            headers = {"Authorization": f"Bearer {self.access_token}", "Content-Type": "application/json"}

            # Simple text message (requires 24h window or approved template)
            payload = {
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": self.to_number,
                "type": "text",
                "text": {"body": f"🤖 Claude Agent '{agent_name}' is online! Container: {container_id[:8]}"},
            }

            response = requests.post(url, json=payload, headers=headers)
            if response.status_code == 200:
                logger.info(f"Sent WhatsApp invitation for agent {agent_name}")
                return True
            else:
                logger.error(f"WhatsApp API error: {response.status_code} - {response.text}")
                return False
        except Exception as e:
            logger.error(f"Failed to send WhatsApp invitation: {e}")
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
            import requests

            url = f"{self.base_url}/{self.phone_number_id}/messages"
            headers = {"Authorization": f"Bearer {self.access_token}", "Content-Type": "application/json"}

            message = f"📊 Agent '{agent_name}': {status}"
            if details:
                for key, value in list(details.items())[:2]:
                    message += f"\n{key}: {value}"

            payload = {"messaging_product": "whatsapp", "recipient_type": "individual", "to": self.to_number, "type": "text", "text": {"body": message}}

            response = requests.post(url, json=payload, headers=headers)
            if response.status_code == 200:
                logger.info(f"Sent WhatsApp status update for agent {agent_name}")
                return True
            else:
                logger.error(f"WhatsApp API error: {response.status_code} - {response.text}")
                return False
        except Exception as e:
            logger.error(f"Failed to send WhatsApp status: {e}")
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
            import requests

            url = f"{self.base_url}/{self.phone_number_id}/messages"
            headers = {"Authorization": f"Bearer {self.access_token}", "Content-Type": "application/json"}

            message = f"✅ Agent '{agent_name}' completed\nTasks: {summary.get('tasks_completed', 0)}\nDuration: {summary.get('duration', 'N/A')}"

            payload = {"messaging_product": "whatsapp", "recipient_type": "individual", "to": self.to_number, "type": "text", "text": {"body": message}}

            response = requests.post(url, json=payload, headers=headers)
            if response.status_code == 200:
                logger.info(f"Sent WhatsApp cleanup notification for agent {agent_name}")
                return True
            else:
                logger.error(f"WhatsApp API error: {response.status_code} - {response.text}")
                return False
        except Exception as e:
            logger.error(f"Failed to send WhatsApp cleanup: {e}")
            return False

    async def receive_message(self, timeout: int = 60) -> Optional[str]:
        """Receive message from user.

        Note: WhatsApp receiving requires webhook setup in Meta developer console.
        This is a placeholder for webhook-based message reception.

        Args:
            timeout: Timeout in seconds

        Returns:
            Message text if received, None otherwise
        """
        logger.warning("WhatsApp message reception requires webhook configuration in Meta developer console")
        return None
