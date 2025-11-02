"""Configuration for Telegram Bot API implementations.

This module provides configuration classes for selecting and configuring
different Telegram API implementations (real, fake, mock).
"""

import contextlib
import os
from dataclasses import dataclass
from typing import Literal


@dataclass
class TelegramAPIConfig:
    """Configuration for Telegram API implementation.

    Attributes:
        implementation: API implementation type ("real", "fake", or "mock")
        bot_token: Telegram bot token (required for "real" mode)
        auto_detect_from_env: Whether to auto-detect settings from environment variables
        fake_delay_ms: Delay in milliseconds for fake implementation (simulates network latency)
        fake_error_rate: Error rate (0.0 to 1.0) for fake implementation testing
    """

    implementation: Literal["real", "fake", "mock"] = "real"
    bot_token: str | None = None
    auto_detect_from_env: bool = True
    fake_delay_ms: int = 100
    fake_error_rate: float = 0.0

    def __post_init__(self) -> None:
        """Post-initialization processing to load from environment."""
        if self.auto_detect_from_env:
            self._load_from_environment()

        # Validate configuration
        self._validate()

    def _load_from_environment(self) -> None:
        """Load configuration from environment variables.

        Environment variables:
            TELEGRAM_API_MODE: Set to "real", "fake", or "mock"
            TELEGRAM_BOT_TOKEN: Bot token (for real mode)
            TELEGRAM_FAKE_DELAY: Delay in ms for fake mode (default: 100)
            TELEGRAM_FAKE_ERROR_RATE: Error rate 0.0-1.0 for fake mode (default: 0.0)
        """
        # Load API mode
        api_mode = os.environ.get("TELEGRAM_API_MODE")
        if api_mode and api_mode in ("real", "fake", "mock"):
            self.implementation = api_mode  # type: ignore[assignment]

        # Load bot token
        bot_token = os.environ.get("TELEGRAM_BOT_TOKEN")
        if bot_token:
            self.bot_token = bot_token

        # Load fake implementation settings
        fake_delay = os.environ.get("TELEGRAM_FAKE_DELAY")
        if fake_delay:
            with contextlib.suppress(ValueError):
                self.fake_delay_ms = int(fake_delay)

        fake_error_rate = os.environ.get("TELEGRAM_FAKE_ERROR_RATE")
        if fake_error_rate:
            with contextlib.suppress(ValueError):
                rate = float(fake_error_rate)
                if 0.0 <= rate <= 1.0:
                    self.fake_error_rate = rate

    def _validate(self) -> None:
        """Validate configuration.

        Raises:
            ValueError: If configuration is invalid
        """
        # Validate implementation type
        if self.implementation not in ("real", "fake", "mock"):
            msg = f"Invalid implementation type: {self.implementation}"
            raise ValueError(msg)

        # Validate real mode has bot token
        if self.implementation == "real" and not self.bot_token:
            msg = "bot_token is required for 'real' implementation mode"
            raise ValueError(msg)

        # Validate fake_delay_ms is positive
        if self.fake_delay_ms < 0:
            msg = f"fake_delay_ms must be non-negative, got {self.fake_delay_ms}"
            raise ValueError(msg)

        # Validate fake_error_rate is in valid range
        if not 0.0 <= self.fake_error_rate <= 1.0:
            msg = f"fake_error_rate must be between 0.0 and 1.0, got {self.fake_error_rate}"
            raise ValueError(msg)

    @classmethod
    def from_environment(cls) -> "TelegramAPIConfig":
        """Create configuration from environment variables.

        Returns:
            TelegramAPIConfig loaded from environment
        """
        return cls(auto_detect_from_env=True)

    @classmethod
    def for_testing(cls, implementation: Literal["fake", "mock"] = "fake") -> "TelegramAPIConfig":
        """Create configuration for testing purposes.

        Args:
            implementation: Implementation type for testing ("fake" or "mock")

        Returns:
            TelegramAPIConfig configured for testing
        """
        if implementation not in ("fake", "mock"):
            msg = "for_testing() only supports 'fake' or 'mock' implementation"
            raise ValueError(msg)

        return cls(
            implementation=implementation,
            bot_token=None,
            auto_detect_from_env=False,
            fake_delay_ms=0,  # No delay for faster tests
            fake_error_rate=0.0,
        )

    @classmethod
    def for_real_bot(cls, bot_token: str) -> "TelegramAPIConfig":
        """Create configuration for real Telegram bot.

        Args:
            bot_token: Telegram bot token

        Returns:
            TelegramAPIConfig configured for real bot
        """
        return cls(
            implementation="real",
            bot_token=bot_token,
            auto_detect_from_env=False,
        )


__all__ = ["TelegramAPIConfig"]
