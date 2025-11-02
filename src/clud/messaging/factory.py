"""Factory for creating Telegram messenger instances."""

import logging

from clud.telegram.api_interface import TelegramBotAPI

from .telegram import TelegramMessenger

logger = logging.getLogger(__name__)


def create_telegram_messenger(bot_token: str, chat_id: str, api: TelegramBotAPI | None = None) -> TelegramMessenger:
    """Create Telegram messenger instance.

    Args:
        bot_token: Telegram bot token
        chat_id: Telegram chat ID
        api: Optional TelegramBotAPI instance for abstraction (enables testing with fakes/mocks)

    Returns:
        TelegramMessenger instance

    Raises:
        ValueError: If bot_token or chat_id is missing
    """
    if not bot_token:
        raise ValueError("Telegram bot_token is required")
    if not chat_id:
        raise ValueError("Telegram chat_id is required")

    logger.info("Creating Telegram messenger")
    return TelegramMessenger(bot_token=bot_token, chat_id=chat_id, api=api)


def validate_telegram_config(bot_token: str, chat_id: str) -> tuple[bool, str]:
    """Validate Telegram configuration.

    Args:
        bot_token: Telegram bot token
        chat_id: Telegram chat ID

    Returns:
        Tuple of (is_valid, error_message)
    """
    if not bot_token:
        return False, "Missing bot_token"
    if not chat_id:
        return False, "Missing chat_id"
    return True, ""
