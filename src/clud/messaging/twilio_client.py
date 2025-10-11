"""Twilio SMS and WhatsApp client implementation."""

import asyncio
import logging
from typing import Any

logger = logging.getLogger(__name__)


class TwilioClient:
    """Twilio SMS and WhatsApp client.

    Note: This class is defined but requires twilio package to be installed.
    Import errors are handled gracefully at the factory level.
    """

    def __init__(self, account_sid: str, auth_token: str, from_number: str) -> None:
        """Initialize Twilio client.

        Args:
            account_sid: Twilio Account SID
            auth_token: Twilio Auth Token
            from_number: Twilio phone number to send from (format: +1234567890)
        """
        try:
            from twilio.rest import Client as TwilioRestClient
            from twilio.base.exceptions import TwilioException

            self.TwilioRestClient = TwilioRestClient
            self.TwilioException = TwilioException
            self.client = TwilioRestClient(account_sid, auth_token)
            self.from_number = from_number
            self._available = True
        except ImportError:
            logger.warning("twilio package not installed. Install with: pip install twilio")
            self._available = False

    async def send_message(self, contact: str, message: str) -> bool:
        """Send SMS or WhatsApp message.

        Args:
            contact: Phone number (+1234567890) or whatsapp:+1234567890
            message: Text to send (max 1600 chars for SMS)

        Returns:
            True if sent successfully
        """
        if not self._available:
            logger.error("Twilio client not available (missing twilio package)")
            return False

        try:
            # Truncate message to SMS limit
            truncated_message = message[:1600]
            if len(message) > 1600:
                truncated_message += "\n\n[Message truncated]"

            # Twilio API is synchronous, wrap in executor
            loop = asyncio.get_event_loop()
            await loop.run_in_executor(
                None,
                lambda: self.client.messages.create(body=truncated_message, from_=self._format_from_number(contact), to=contact),
            )

            channel = "WhatsApp" if contact.startswith("whatsapp:") else "SMS"
            logger.info(f"{channel} message sent to {contact}")
            return True

        except self.TwilioException as e:
            logger.error(f"Twilio send failed to {contact}: {e}")
            return False
        except Exception as e:
            logger.error(f"Unexpected error sending Twilio message: {e}")
            return False

    async def send_code_block(self, contact: str, code: str, language: str = "python") -> bool:
        """Send code block (plain text for SMS/WhatsApp).

        Args:
            contact: Phone number or whatsapp: prefixed number
            code: Code content
            language: Language hint (included in message)

        Returns:
            True if sent successfully
        """
        # SMS/WhatsApp don't support rich formatting, use plain text
        formatted = f"[{language} code]\n{code}"
        return await self.send_message(contact, formatted)

    def _format_from_number(self, contact: str) -> str:
        """Format from_number based on destination channel.

        Args:
            contact: Destination contact string

        Returns:
            Properly formatted from_number for Twilio API
        """
        if contact.startswith("whatsapp:"):
            return f"whatsapp:{self.from_number}"
        return self.from_number

    def is_available(self) -> bool:
        """Check if Twilio client is available."""
        return self._available
