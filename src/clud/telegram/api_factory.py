"""Factory for creating Telegram Bot API implementations.

This module provides factory functions for creating appropriate Telegram API
implementations based on configuration.
"""

import logging

from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_interface import TelegramBotAPI

logger = logging.getLogger(__name__)


def create_telegram_api(
    config: TelegramAPIConfig | None = None,
    bot_token: str | None = None,
) -> TelegramBotAPI:
    """Create appropriate Telegram API implementation based on config.

    This factory function creates the right implementation (real, fake, or mock)
    based on the provided configuration or environment variables.

    Args:
        config: Optional TelegramAPIConfig. If None, will auto-detect from environment.
        bot_token: Optional bot token. If provided, overrides config bot_token.

    Returns:
        TelegramBotAPI implementation instance

    Raises:
        ValueError: If configuration is invalid
        ImportError: If required dependencies are missing
    """
    # Auto-detect config from environment if not provided
    if config is None:
        config = TelegramAPIConfig.from_environment()

    # Override bot_token if provided
    if bot_token is not None:
        config.bot_token = bot_token

    # Auto-detect implementation mode if not explicitly set
    # If no token provided and mode is real, switch to fake for testing
    if config.auto_detect_from_env and config.implementation == "real" and not config.bot_token:
        logger.info("No bot token provided, switching to 'fake' mode for testing")
        config.implementation = "fake"

    # Create appropriate implementation
    if config.implementation == "real":
        return _create_real_api(config)
    elif config.implementation == "fake":
        return _create_fake_api(config)
    elif config.implementation == "mock":
        return _create_mock_api(config)
    else:
        msg = f"Unknown implementation type: {config.implementation}"
        raise ValueError(msg)


def _create_real_api(config: TelegramAPIConfig) -> TelegramBotAPI:
    """Create real Telegram API implementation.

    Args:
        config: Configuration with bot token

    Returns:
        RealTelegramBotAPI instance

    Raises:
        ValueError: If bot token is missing
        ImportError: If python-telegram-bot is not installed
    """
    if not config.bot_token:
        msg = "bot_token is required for real Telegram API"
        raise ValueError(msg)

    try:
        from clud.telegram.api_real import RealTelegramBotAPI

        logger.info("Creating real Telegram API implementation")
        return RealTelegramBotAPI(bot_token=config.bot_token)
    except ImportError as e:
        msg = "python-telegram-bot is required for real Telegram API. Install with: pip install python-telegram-bot"
        raise ImportError(msg) from e


def _create_fake_api(config: TelegramAPIConfig) -> TelegramBotAPI:
    """Create fake Telegram API implementation for testing.

    Args:
        config: Configuration with fake settings

    Returns:
        FakeTelegramBotAPI instance
    """
    from clud.telegram.api_fake import FakeTelegramBotAPI

    logger.info(f"Creating fake Telegram API implementation (delay={config.fake_delay_ms}ms, error_rate={config.fake_error_rate})")
    return FakeTelegramBotAPI(config=config)


def _create_mock_api(config: TelegramAPIConfig) -> TelegramBotAPI:
    """Create mock Telegram API implementation for unit testing.

    Args:
        config: Configuration (not used for mock)

    Returns:
        MockTelegramBotAPI instance
    """
    from tests.mocks.telegram_api import MockTelegramBotAPI

    logger.info("Creating mock Telegram API implementation")
    return MockTelegramBotAPI()


__all__ = ["create_telegram_api"]
