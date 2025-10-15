"""Configuration management for Telegram integration.

This module provides configuration loading and validation for the Telegram bot
integration, supporting environment variables and configuration files.
"""

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml


@dataclass
class TelegramConfig:
    """Telegram bot configuration."""

    bot_token: str
    webhook_url: str | None = None
    allowed_users: list[int] = field(default_factory=list)  # pyright: ignore[reportUnknownVariableType]
    polling: bool = True


@dataclass
class WebConfig:
    """Web interface configuration."""

    port: int = 8889
    host: str = "127.0.0.1"
    auth_required: bool = False
    auth_token: str | None = None
    bidirectional: bool = False


@dataclass
class SessionConfig:
    """Session management configuration."""

    timeout_seconds: int = 3600
    max_sessions: int = 50
    message_history_limit: int = 1000
    cleanup_interval: int = 300


@dataclass
class LoggingConfig:
    """Logging configuration."""

    level: str = "INFO"
    file: str | None = None


@dataclass
class TelegramIntegrationConfig:
    """Main configuration for Telegram integration."""

    telegram: TelegramConfig
    web: WebConfig = field(default_factory=WebConfig)
    sessions: SessionConfig = field(default_factory=SessionConfig)
    logging: LoggingConfig = field(default_factory=LoggingConfig)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "TelegramIntegrationConfig":
        """Create configuration from dictionary.

        Args:
            data: Configuration dictionary

        Returns:
            TelegramIntegrationConfig instance
        """
        # Parse telegram config
        telegram_data = data.get("telegram", {})
        telegram = TelegramConfig(
            bot_token=telegram_data.get("bot_token", ""),
            webhook_url=telegram_data.get("webhook_url"),
            allowed_users=telegram_data.get("allowed_users", []),
            polling=telegram_data.get("polling", True),
        )

        # Parse web config
        web_data = data.get("web", {})
        web = WebConfig(
            port=web_data.get("port", 8889),
            host=web_data.get("host", "127.0.0.1"),
            auth_required=web_data.get("auth_required", False),
            auth_token=web_data.get("auth_token"),
            bidirectional=web_data.get("bidirectional", False),
        )

        # Parse sessions config
        sessions_data = data.get("sessions", {})
        sessions = SessionConfig(
            timeout_seconds=sessions_data.get("timeout_seconds", 3600),
            max_sessions=sessions_data.get("max_sessions", 50),
            message_history_limit=sessions_data.get("message_history_limit", 1000),
            cleanup_interval=sessions_data.get("cleanup_interval", 300),
        )

        # Parse logging config
        logging_data = data.get("logging", {})
        logging_cfg = LoggingConfig(level=logging_data.get("level", "INFO"), file=logging_data.get("file"))

        return cls(telegram=telegram, web=web, sessions=sessions, logging=logging_cfg)

    @classmethod
    def from_env(cls) -> "TelegramIntegrationConfig":
        """Create configuration from environment variables.

        Returns:
            TelegramIntegrationConfig instance with values from environment

        Raises:
            ValueError: If required environment variables are missing
        """
        bot_token = os.getenv("TELEGRAM_BOT_TOKEN", "")

        # Fall back to keyring if environment variable not set
        if not bot_token:
            from ..agent_cli import load_telegram_credentials

            bot_token_keyring, _ = load_telegram_credentials()
            if bot_token_keyring:
                bot_token = bot_token_keyring

        if not bot_token:
            raise ValueError("TELEGRAM_BOT_TOKEN environment variable or stored credentials required")

        # Parse allowed users
        allowed_users_str = os.getenv("TELEGRAM_ALLOWED_USERS", "")
        allowed_users = []
        if allowed_users_str:
            try:
                allowed_users = [int(uid.strip()) for uid in allowed_users_str.split(",") if uid.strip()]
            except ValueError as e:
                raise ValueError(f"Invalid TELEGRAM_ALLOWED_USERS format: {e}") from e

        telegram = TelegramConfig(
            bot_token=bot_token,
            webhook_url=os.getenv("TELEGRAM_WEBHOOK_URL"),
            allowed_users=allowed_users,
            polling=os.getenv("TELEGRAM_WEBHOOK_URL") is None,  # Use polling if no webhook
        )

        web = WebConfig(
            port=int(os.getenv("TELEGRAM_WEB_PORT", "8889")),
            host=os.getenv("TELEGRAM_WEB_HOST", "127.0.0.1"),
            auth_required=os.getenv("TELEGRAM_WEB_AUTH") is not None,
            auth_token=os.getenv("TELEGRAM_WEB_AUTH"),
            bidirectional=os.getenv("TELEGRAM_BIDIRECTIONAL", "").lower() == "true",
        )

        sessions = SessionConfig(
            timeout_seconds=int(os.getenv("TELEGRAM_SESSION_TIMEOUT", "3600")),
            max_sessions=int(os.getenv("TELEGRAM_MAX_SESSIONS", "50")),
            message_history_limit=int(os.getenv("TELEGRAM_MESSAGE_HISTORY_LIMIT", "1000")),
            cleanup_interval=int(os.getenv("TELEGRAM_CLEANUP_INTERVAL", "300")),
        )

        logging_cfg = LoggingConfig(level=os.getenv("TELEGRAM_LOG_LEVEL", "INFO"), file=os.getenv("TELEGRAM_LOG_FILE"))

        return cls(telegram=telegram, web=web, sessions=sessions, logging=logging_cfg)

    @classmethod
    def from_file(cls, file_path: Path | str) -> "TelegramIntegrationConfig":
        """Load configuration from YAML file.

        Args:
            file_path: Path to configuration file

        Returns:
            TelegramIntegrationConfig instance

        Raises:
            FileNotFoundError: If config file not found
            ValueError: If config file is invalid
        """
        file_path = Path(file_path)
        if not file_path.exists():
            raise FileNotFoundError(f"Configuration file not found: {file_path}")

        try:
            with open(file_path) as f:
                data = yaml.safe_load(f)
                if not data:
                    raise ValueError("Configuration file is empty")
                return cls.from_dict(data)
        except yaml.YAMLError as e:
            raise ValueError(f"Invalid YAML in configuration file: {e}") from e

    @classmethod
    def load(cls, config_file: Path | str | None = None) -> "TelegramIntegrationConfig":
        """Load configuration with precedence: file > env > defaults.

        Args:
            config_file: Optional path to configuration file

        Returns:
            TelegramIntegrationConfig instance

        Raises:
            ValueError: If configuration is invalid
        """
        # Try loading from file if provided
        if config_file:
            return cls.from_file(config_file)

        # Try loading from environment
        try:
            return cls.from_env()
        except ValueError as e:
            raise ValueError(f"Failed to load configuration: {e}") from e

    def validate(self) -> list[str]:
        """Validate the configuration.

        Returns:
            List of validation error messages (empty if valid)
        """
        errors: list[str] = []

        # Validate bot token
        if not self.telegram.bot_token:
            errors.append("Telegram bot token is required")

        # Validate web config
        if self.web.port < 1 or self.web.port > 65535:
            errors.append(f"Invalid web port: {self.web.port}")

        if self.web.auth_required and not self.web.auth_token:
            errors.append("Auth token is required when auth_required is True")

        # Validate session config
        if self.sessions.max_sessions < 1:
            errors.append("max_sessions must be at least 1")

        if self.sessions.timeout_seconds < 60:
            errors.append("timeout_seconds must be at least 60")

        if self.sessions.message_history_limit < 1:
            errors.append("message_history_limit must be at least 1")

        return errors

    def to_dict(self) -> dict[str, Any]:
        """Convert configuration to dictionary.

        Returns:
            Dictionary representation
        """
        return {
            "telegram": {
                "bot_token": "***" if self.telegram.bot_token else "",  # Mask token
                "webhook_url": self.telegram.webhook_url,
                "allowed_users": self.telegram.allowed_users,
                "polling": self.telegram.polling,
            },
            "web": {
                "port": self.web.port,
                "host": self.web.host,
                "auth_required": self.web.auth_required,
                "bidirectional": self.web.bidirectional,
            },
            "sessions": {
                "timeout_seconds": self.sessions.timeout_seconds,
                "max_sessions": self.sessions.max_sessions,
                "message_history_limit": self.sessions.message_history_limit,
                "cleanup_interval": self.sessions.cleanup_interval,
            },
            "logging": {"level": self.logging.level, "file": self.logging.file},
        }
