"""Factory for creating messaging platform instances."""

import logging
from typing import Any

from .sms import SMSMessenger
from .telegram import TelegramMessenger
from .whatsapp import WhatsAppMessenger

logger = logging.getLogger(__name__)


class MessengerFactory:
    """Factory for creating messenger instances based on platform."""

    @staticmethod
    def create_messenger(platform: str, config: dict[str, Any]) -> Any:
        """Create appropriate messenger based on platform.

        Args:
            platform: Platform name ('telegram', 'sms', or 'whatsapp')
            config: Configuration dictionary for the platform

        Returns:
            Messenger instance

        Raises:
            ValueError: If platform is not supported or config is invalid
        """
        platform = platform.lower()

        if platform == "telegram":
            bot_token = config.get("bot_token")
            chat_id = config.get("chat_id")

            if not bot_token:
                raise ValueError("Telegram config missing 'bot_token'")
            if not chat_id:
                raise ValueError("Telegram config missing 'chat_id'")

            logger.info("Creating Telegram messenger")
            return TelegramMessenger(bot_token=bot_token, chat_id=chat_id)

        elif platform == "sms":
            account_sid = config.get("account_sid")
            auth_token = config.get("auth_token")
            from_number = config.get("from_number")
            to_number = config.get("to_number")

            if not account_sid:
                raise ValueError("SMS config missing 'account_sid'")
            if not auth_token:
                raise ValueError("SMS config missing 'auth_token'")
            if not from_number:
                raise ValueError("SMS config missing 'from_number'")
            if not to_number:
                raise ValueError("SMS config missing 'to_number'")

            logger.info("Creating SMS messenger")
            return SMSMessenger(account_sid=account_sid, auth_token=auth_token, from_number=from_number, to_number=to_number)

        elif platform == "whatsapp":
            phone_number_id = config.get("phone_number_id")
            access_token = config.get("access_token")
            to_number = config.get("to_number")

            if not phone_number_id:
                raise ValueError("WhatsApp config missing 'phone_number_id'")
            if not access_token:
                raise ValueError("WhatsApp config missing 'access_token'")
            if not to_number:
                raise ValueError("WhatsApp config missing 'to_number'")

            logger.info("Creating WhatsApp messenger")
            return WhatsAppMessenger(phone_number_id=phone_number_id, access_token=access_token, to_number=to_number)

        else:
            raise ValueError(f"Unsupported messaging platform: {platform}. Supported: telegram, sms, whatsapp")

    @staticmethod
    def validate_config(platform: str, config: dict[str, Any]) -> tuple[bool, str]:
        """Validate configuration for a platform.

        Args:
            platform: Platform name
            config: Configuration dictionary

        Returns:
            Tuple of (is_valid, error_message)
        """
        try:
            MessengerFactory.create_messenger(platform, config)
            return True, ""
        except ValueError as e:
            return False, str(e)
        except Exception as e:
            return False, f"Unexpected error: {e}"
