"""Configuration management for messaging credentials."""

import json
import logging
import os
import platform
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


def get_messaging_config_file() -> Path:
    """Get path to messaging configuration file.

    Returns:
        Path to ~/.clud/messaging.json
    """
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(exist_ok=True)
    return config_dir / "messaging.json"


def load_messaging_config() -> dict[str, Any]:
    """Load messaging configuration from environment and config file.

    Priority order:
    1. Environment variables (TELEGRAM_BOT_TOKEN, TWILIO_*)
    2. Config file (~/.clud/messaging.json)
    3. Individual key files (~/.clud/*.key)

    Returns:
        Configuration dictionary with keys:
        - telegram_token (optional)
        - twilio_sid (optional)
        - twilio_token (optional)
        - twilio_number (optional)
    """
    config: dict[str, Any] = {}

    # Load from config file
    config_file = get_messaging_config_file()
    if config_file.exists():
        try:
            with open(config_file, encoding="utf-8") as f:
                file_config = json.load(f)

            # Extract nested values
            if "telegram" in file_config and isinstance(file_config["telegram"], dict):
                config["telegram_token"] = file_config["telegram"].get("bot_token")

            if "twilio" in file_config and isinstance(file_config["twilio"], dict):
                config["twilio_sid"] = file_config["twilio"].get("account_sid")
                config["twilio_token"] = file_config["twilio"].get("auth_token")
                config["twilio_number"] = file_config["twilio"].get("from_number")

        except (json.JSONDecodeError, OSError) as e:
            logger.warning(f"Failed to load messaging config from {config_file}: {e}")

    # Load from individual key files (backward compatibility)
    clud_dir = Path.home() / ".clud"
    if clud_dir.exists():
        telegram_key_file = clud_dir / "telegram-bot-token.key"
        if telegram_key_file.exists():
            try:
                config["telegram_token"] = telegram_key_file.read_text(encoding="utf-8").strip()
            except OSError:
                pass

    # Environment variables override everything
    if os.environ.get("TELEGRAM_BOT_TOKEN"):
        config["telegram_token"] = os.environ["TELEGRAM_BOT_TOKEN"].strip()

    if os.environ.get("TWILIO_ACCOUNT_SID"):
        config["twilio_sid"] = os.environ["TWILIO_ACCOUNT_SID"].strip()

    if os.environ.get("TWILIO_AUTH_TOKEN"):
        config["twilio_token"] = os.environ["TWILIO_AUTH_TOKEN"].strip()

    if os.environ.get("TWILIO_FROM_NUMBER"):
        config["twilio_number"] = os.environ["TWILIO_FROM_NUMBER"].strip()

    # Remove None values
    config = {k: v for k, v in config.items() if v}

    return config


def save_messaging_config(config: dict[str, Any]) -> None:
    """Save messaging configuration to file.

    Args:
        config: Configuration dictionary with telegram/twilio settings
    """
    config_file = get_messaging_config_file()

    # Load existing config to merge
    existing: dict[str, Any] = {}
    if config_file.exists():
        try:
            with open(config_file, encoding="utf-8") as f:
                existing = json.load(f)
        except (json.JSONDecodeError, OSError):
            existing = {}

    # Merge new config
    if "telegram_token" in config:
        if "telegram" not in existing:
            existing["telegram"] = {}
        existing["telegram"]["bot_token"] = config["telegram_token"]
        existing["telegram"]["enabled"] = True

    if any(k in config for k in ["twilio_sid", "twilio_token", "twilio_number"]):
        if "twilio" not in existing:
            existing["twilio"] = {}
        if "twilio_sid" in config:
            existing["twilio"]["account_sid"] = config["twilio_sid"]
        if "twilio_token" in config:
            existing["twilio"]["auth_token"] = config["twilio_token"]
        if "twilio_number" in config:
            existing["twilio"]["from_number"] = config["twilio_number"]
        existing["twilio"]["enabled"] = True

    # Write config file
    try:
        with open(config_file, "w", encoding="utf-8") as f:
            json.dump(existing, f, indent=2)

        # Set restrictive permissions
        if platform.system() != "Windows":
            config_file.chmod(0o600)

        logger.info(f"Messaging configuration saved to {config_file}")

    except OSError as e:
        logger.error(f"Failed to save messaging config: {e}")
        raise


def prompt_for_messaging_config() -> dict[str, Any]:
    """Interactively prompt user for messaging credentials.

    Returns:
        Configuration dictionary
    """
    import sys

    config: dict[str, Any] = {}

    print("\n=== Messaging Configuration ===")
    print("Configure Telegram, SMS, and/or WhatsApp notifications")
    print()

    # Telegram
    print("Telegram Bot Configuration (optional)")
    print("Get token from @BotFather: https://t.me/botfather")
    sys.stdout.flush()
    telegram_token = input("Telegram Bot Token (or press Enter to skip): ").strip()
    if telegram_token:
        config["telegram_token"] = telegram_token
        print("✓ Telegram configured")
    print()

    # Twilio (SMS + WhatsApp)
    print("Twilio Configuration for SMS/WhatsApp (optional)")
    print("Get credentials from: https://www.twilio.com/console")
    sys.stdout.flush()
    twilio_sid = input("Twilio Account SID (or press Enter to skip): ").strip()
    if twilio_sid:
        config["twilio_sid"] = twilio_sid

        sys.stdout.flush()
        twilio_token = input("Twilio Auth Token: ").strip()
        config["twilio_token"] = twilio_token

        sys.stdout.flush()
        twilio_number = input("Twilio Phone Number (format: +1234567890): ").strip()
        config["twilio_number"] = twilio_number

        print("✓ Twilio configured (SMS + WhatsApp)")
    print()

    if not config:
        print("No credentials configured. You can configure later using environment variables.")
        return {}

    # Save config
    try:
        save_messaging_config(config)
        print(f"\n✓ Configuration saved to {get_messaging_config_file()}")
        print("\nYou can now use --notify-user to receive agent status updates!")
    except Exception as e:
        print(f"\n✗ Failed to save configuration: {e}", file=sys.stderr)

    return config
