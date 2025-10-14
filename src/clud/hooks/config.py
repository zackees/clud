"""Hook configuration management.

This module provides configuration loading and validation for the hook system,
supporting both .clud configuration files and environment variables.
"""

import logging
import os
from dataclasses import dataclass
from pathlib import Path

logger = logging.getLogger(__name__)


@dataclass
class HookConfig:
    """Configuration for hook handlers.

    Attributes:
        enabled: Whether hooks are enabled
        telegram_enabled: Whether Telegram hooks are enabled
        telegram_bot_token: Telegram bot API token
        telegram_chat_id: Default Telegram chat ID for hooks
        webhook_enabled: Whether webhook hooks are enabled
        webhook_url: URL to send webhook notifications
        webhook_secret: Secret for webhook authentication
        buffer_size: Maximum buffer size for output chunks
        flush_interval: Time in seconds before auto-flushing buffers
    """

    enabled: bool = False
    telegram_enabled: bool = False
    telegram_bot_token: str = ""
    telegram_chat_id: str = ""
    webhook_enabled: bool = False
    webhook_url: str = ""
    webhook_secret: str = ""
    buffer_size: int = 2000
    flush_interval: float = 2.0

    def validate(self) -> list[str]:
        """Validate the configuration.

        Returns:
            List of validation error messages (empty if valid)
        """
        errors: list[str] = []

        if self.enabled:
            if self.telegram_enabled:
                if not self.telegram_bot_token:
                    errors.append("Telegram hook enabled but telegram_bot_token not provided")
                if not self.telegram_chat_id:
                    errors.append("Telegram hook enabled but telegram_chat_id not provided")

            if self.webhook_enabled and not self.webhook_url:
                errors.append("Webhook hook enabled but webhook_url not provided")

            if self.buffer_size <= 0:
                errors.append("buffer_size must be positive")

            if self.flush_interval <= 0:
                errors.append("flush_interval must be positive")

        return errors

    def is_valid(self) -> bool:
        """Check if the configuration is valid.

        Returns:
            True if configuration is valid, False otherwise
        """
        return len(self.validate()) == 0


def load_hook_config(config_file: Path | str | None = None) -> HookConfig:
    """Load hook configuration from file and environment variables.

    Configuration priority (highest to lowest):
    1. Environment variables
    2. Configuration file (.clud or specified file)
    3. Default values

    Args:
        config_file: Optional path to configuration file

    Returns:
        HookConfig instance with loaded configuration
    """
    config = HookConfig()

    # Try to load from file if specified or if .clud exists
    if config_file:
        _load_from_file(config, Path(config_file))
    else:
        # Check for .clud in current directory and parent directories
        current_dir = Path.cwd()
        for parent in [current_dir] + list(current_dir.parents):
            clud_path = parent / ".clud"
            if clud_path.exists():
                # If .clud is a file, use it directly
                if clud_path.is_file():
                    _load_from_file(config, clud_path)
                    break
                # If .clud is a directory, look for config file inside
                elif clud_path.is_dir():
                    # Try common config file names
                    for config_name in ["config", "config.ini", "config.yaml", "config.yml"]:
                        config_file_path = clud_path / config_name
                        if config_file_path.is_file():
                            _load_from_file(config, config_file_path)
                            break
                    break

    # Override with environment variables
    _load_from_env(config)

    # Log validation errors if any
    errors = config.validate()
    if errors:
        for error in errors:
            logger.warning(f"Hook configuration validation error: {error}")

    return config


def _load_from_file(config: HookConfig, config_file: Path) -> None:
    """Load configuration from a file.

    Args:
        config: HookConfig instance to update
        config_file: Path to configuration file
    """
    try:
        if not config_file.exists():
            logger.debug(f"Configuration file not found: {config_file}")
            return

        # Check if it's actually a file (not a directory)
        if not config_file.is_file():
            logger.debug(f"Configuration path is not a file: {config_file}")
            return

        # Read the file
        content = config_file.read_text(encoding="utf-8")

        # Parse simple key=value format
        for line in content.splitlines():
            line = line.strip()

            # Skip comments and empty lines
            if not line or line.startswith("#"):
                continue

            # Parse key=value
            if "=" in line:
                key, value = line.split("=", 1)
                key = key.strip().lower()
                value = value.strip().strip('"').strip("'")

                _set_config_value(config, key, value)

        logger.debug(f"Loaded hook configuration from {config_file}")

    except Exception as e:
        logger.error(f"Error loading configuration from {config_file}: {e}", exc_info=True)


def _load_from_env(config: HookConfig) -> None:
    """Load configuration from environment variables.

    Args:
        config: HookConfig instance to update
    """
    env_mapping = {
        "CLUD_HOOKS_ENABLED": "enabled",
        "CLUD_HOOKS_TELEGRAM_ENABLED": "telegram_enabled",
        "TELEGRAM_BOT_TOKEN": "telegram_bot_token",
        "TELEGRAM_CHAT_ID": "telegram_chat_id",
        "CLUD_HOOKS_WEBHOOK_ENABLED": "webhook_enabled",
        "CLUD_HOOKS_WEBHOOK_URL": "webhook_url",
        "CLUD_HOOKS_WEBHOOK_SECRET": "webhook_secret",
        "CLUD_HOOKS_BUFFER_SIZE": "buffer_size",
        "CLUD_HOOKS_FLUSH_INTERVAL": "flush_interval",
    }

    for env_var, config_key in env_mapping.items():
        value = os.environ.get(env_var)
        if value is not None:
            _set_config_value(config, config_key, value)


def _set_config_value(config: HookConfig, key: str, value: str) -> None:
    """Set a configuration value with type conversion.

    Args:
        config: HookConfig instance to update
        key: Configuration key
        value: Value to set (as string)
    """
    # Map common key variations
    key_mapping = {
        "hooks_enabled": "enabled",
        "telegram.enabled": "telegram_enabled",
        "telegram.bot_token": "telegram_bot_token",
        "telegram.chat_id": "telegram_chat_id",
        "webhook.enabled": "webhook_enabled",
        "webhook.url": "webhook_url",
        "webhook.secret": "webhook_secret",
        "hooks.buffer_size": "buffer_size",
        "hooks.flush_interval": "flush_interval",
    }

    key = key_mapping.get(key, key)

    # Set the value with appropriate type conversion
    if hasattr(config, key):
        current_value = getattr(config, key)

        # Convert to appropriate type
        if isinstance(current_value, bool):
            converted_value = value.lower() in ("true", "1", "yes", "on")
        elif isinstance(current_value, int):
            converted_value = int(value)
        elif isinstance(current_value, float):
            converted_value = float(value)
        else:
            converted_value = value

        setattr(config, key, converted_value)
        logger.debug(f"Set hook config {key} = {converted_value}")
    else:
        logger.warning(f"Unknown hook configuration key: {key}")


__all__ = ["HookConfig", "load_hook_config"]
