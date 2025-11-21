"""Configuration management for cron scheduler.

This module handles reading and writing cron configuration to ~/.clud/cron.json.
"""

import json
import logging
from pathlib import Path

from clud.cron.models import CronConfig

logger = logging.getLogger(__name__)


class CronConfigManager:
    """Manages cron configuration persistence."""

    def __init__(self, config_path: Path | None = None) -> None:
        """Initialize config manager.

        Args:
            config_path: Path to config file (defaults to ~/.clud/cron.json)
        """
        if config_path is None:
            config_path = Path.home() / ".clud" / "cron.json"
        self.config_path = config_path

    def load(self) -> CronConfig:
        """Load configuration from file.

        Returns:
            CronConfig instance (empty config if file doesn't exist)
        """
        if not self.config_path.exists():
            logger.info(f"Config file not found: {self.config_path}, returning empty config")
            return CronConfig()

        try:
            with open(self.config_path, encoding="utf-8") as f:
                data = json.load(f)
            logger.debug(f"Loaded config from {self.config_path}")
            return CronConfig.from_dict(data)
        except json.JSONDecodeError as e:
            logger.error(f"Failed to parse config file {self.config_path}: {e}")
            raise ValueError(f"Invalid JSON in config file: {e}") from e
        except Exception as e:
            logger.error(f"Failed to load config from {self.config_path}: {e}")
            raise

    def save(self, config: CronConfig) -> None:
        """Save configuration to file.

        Args:
            config: CronConfig instance to save
        """
        # Create parent directory if it doesn't exist
        self.config_path.parent.mkdir(parents=True, exist_ok=True)

        try:
            with open(self.config_path, "w", encoding="utf-8") as f:
                json.dump(config.to_dict(), f, indent=2)
            logger.debug(f"Saved config to {self.config_path}")
        except Exception as e:
            logger.error(f"Failed to save config to {self.config_path}: {e}")
            raise

    def exists(self) -> bool:
        """Check if config file exists.

        Returns:
            True if config file exists, False otherwise
        """
        return self.config_path.exists()

    def delete(self) -> None:
        """Delete config file if it exists."""
        if self.config_path.exists():
            self.config_path.unlink()
            logger.debug(f"Deleted config file {self.config_path}")
