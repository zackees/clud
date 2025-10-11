"""Factory for creating appropriate messaging client based on contact format."""

import logging
from typing import Any

from .telegram_client import TelegramClient
from .twilio_client import TwilioClient

logger = logging.getLogger(__name__)


def create_client(contact: str, config: dict[str, Any]) -> TelegramClient | TwilioClient | None:
    """Auto-detect and create appropriate messaging client.

    Args:
        contact: User contact string (determines channel)
            - +1234567890 -> SMS (Twilio)
            - whatsapp:+1234567890 -> WhatsApp (Twilio)
            - telegram:123456789 -> Telegram
            - telegram:@username -> Telegram
            - @username -> Telegram (default for @)
            - 123456789 -> Telegram (numeric chat_id)
        config: Configuration dictionary containing:
            - telegram_token: Telegram Bot API token (optional)
            - twilio_sid: Twilio Account SID (optional)
            - twilio_token: Twilio Auth Token (optional)
            - twilio_number: Twilio from phone number (optional)

    Returns:
        Appropriate MessagingClient instance, or None if config missing

    Raises:
        ValueError: If contact format is invalid or required config missing
    """
    # Telegram: telegram: prefix, @ prefix, or plain numeric (chat_id)
    if contact.startswith("telegram:") or contact.startswith("@") or (contact.lstrip("-").isdigit() and not contact.startswith("+")):
        if "telegram_token" not in config:
            raise ValueError("Telegram token not configured. Set TELEGRAM_BOT_TOKEN environment variable or use --configure-messaging")

        client = TelegramClient(config["telegram_token"])
        if not client.is_available():
            raise ValueError("python-telegram-bot package not installed. Install with: pip install python-telegram-bot")

        return client

    # WhatsApp: whatsapp: prefix
    elif contact.startswith("whatsapp:"):
        required = ["twilio_sid", "twilio_token", "twilio_number"]
        missing = [key for key in required if key not in config]
        if missing:
            raise ValueError(f"Twilio configuration missing: {', '.join(missing)}. Set environment variables or use --configure-messaging")

        client = TwilioClient(config["twilio_sid"], config["twilio_token"], config["twilio_number"])
        if not client.is_available():
            raise ValueError("twilio package not installed. Install with: pip install twilio")

        return client

    # SMS: phone number with + prefix
    elif contact.startswith("+"):
        required = ["twilio_sid", "twilio_token", "twilio_number"]
        missing = [key for key in required if key not in config]
        if missing:
            raise ValueError(f"Twilio configuration missing: {', '.join(missing)}. Set environment variables or use --configure-messaging")

        client = TwilioClient(config["twilio_sid"], config["twilio_token"], config["twilio_number"])
        if not client.is_available():
            raise ValueError("twilio package not installed. Install with: pip install twilio")

        return client

    else:
        raise ValueError(
            f"Invalid contact format: '{contact}'. "
            "Valid formats: +1234567890 (SMS), whatsapp:+1234567890 (WhatsApp), "
            "telegram:123456789 (Telegram chat_id), @username (Telegram)"
        )


def validate_contact_format(contact: str) -> tuple[bool, str]:
    """Validate contact format without creating client.

    Args:
        contact: Contact string to validate

    Returns:
        Tuple of (is_valid, channel_name)
    """
    if contact.startswith("telegram:") or contact.startswith("@"):
        return (True, "telegram")
    elif contact.lstrip("-").isdigit() and not contact.startswith("+"):
        return (True, "telegram")
    elif contact.startswith("whatsapp:"):
        return (True, "whatsapp")
    elif contact.startswith("+"):
        return (True, "sms")
    else:
        return (False, "unknown")
