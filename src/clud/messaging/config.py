"""Configuration management for messaging credentials.

Uses clud's existing credential store infrastructure for secure storage.
"""

import json
import logging
import os
import platform
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


def get_messaging_config_file() -> Path:
    """Get path to legacy messaging configuration file.

    Returns:
        Path to ~/.clud/messaging.json (deprecated)
    """
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(exist_ok=True)
    return config_dir / "messaging.json"


def load_messaging_config() -> dict[str, Any]:
    """Load messaging configuration using clud's credential infrastructure.

    Priority order:
    1. Environment variables (TELEGRAM_BOT_TOKEN, TWILIO_*)
    2. Credential store (~/.clud/credentials.enc - encrypted, secure)
    3. Individual .key files (~/.clud/*.key - backward compat)
    4. Legacy JSON file (~/.clud/messaging.json - deprecated, will warn)

    Returns:
        Configuration dictionary with keys:
        - telegram_token (optional)
        - twilio_sid (optional)
        - twilio_token (optional)
        - twilio_number (optional)
    """
    config: dict[str, Any] = {}
    sources_used: list[str] = []

    # Priority 1: Environment variables (highest priority)
    if os.environ.get("TELEGRAM_BOT_TOKEN"):
        config["telegram_token"] = os.environ["TELEGRAM_BOT_TOKEN"].strip()
        sources_used.append("TELEGRAM_BOT_TOKEN env var")

    if os.environ.get("TWILIO_ACCOUNT_SID"):
        config["twilio_sid"] = os.environ["TWILIO_ACCOUNT_SID"].strip()
        sources_used.append("TWILIO_ACCOUNT_SID env var")

    if os.environ.get("TWILIO_AUTH_TOKEN"):
        config["twilio_token"] = os.environ["TWILIO_AUTH_TOKEN"].strip()
        sources_used.append("TWILIO_AUTH_TOKEN env var")

    if os.environ.get("TWILIO_FROM_NUMBER"):
        config["twilio_number"] = os.environ["TWILIO_FROM_NUMBER"].strip()
        sources_used.append("TWILIO_FROM_NUMBER env var")

    # Priority 2: Credential store (encrypted, secure) - NEW!
    try:
        from clud.secrets import get_credential_store

        keyring = get_credential_store()
        if keyring:
            # Try to load from credential store
            if not config.get("telegram_token"):
                token = keyring.get_password("clud", "telegram-bot-token")
                if token:
                    config["telegram_token"] = token
                    sources_used.append("credential store (encrypted)")

            if not config.get("twilio_sid"):
                sid = keyring.get_password("clud", "twilio-account-sid")
                if sid:
                    config["twilio_sid"] = sid
                    sources_used.append("credential store (encrypted)")

            if not config.get("twilio_token"):
                token = keyring.get_password("clud", "twilio-auth-token")
                if token:
                    config["twilio_token"] = token
                    sources_used.append("credential store (encrypted)")

            if not config.get("twilio_number"):
                number = keyring.get_password("clud", "twilio-from-number")
                if number:
                    config["twilio_number"] = number
                    sources_used.append("credential store (encrypted)")
    except ImportError:
        logger.debug("Credential store not available, trying other sources")

    # Priority 3: Individual .key files (backward compatibility)
    clud_dir = Path.home() / ".clud"
    if clud_dir.exists():
        if not config.get("telegram_token"):
            telegram_key_file = clud_dir / "telegram-bot-token.key"
            if telegram_key_file.exists():
                try:
                    config["telegram_token"] = telegram_key_file.read_text(encoding="utf-8").strip()
                    sources_used.append("telegram-bot-token.key")
                except OSError:
                    pass

        if not config.get("twilio_sid"):
            sid_key_file = clud_dir / "twilio-account-sid.key"
            if sid_key_file.exists():
                try:
                    config["twilio_sid"] = sid_key_file.read_text(encoding="utf-8").strip()
                    sources_used.append("twilio-account-sid.key")
                except OSError:
                    pass

        if not config.get("twilio_token"):
            token_key_file = clud_dir / "twilio-auth-token.key"
            if token_key_file.exists():
                try:
                    config["twilio_token"] = token_key_file.read_text(encoding="utf-8").strip()
                    sources_used.append("twilio-auth-token.key")
                except OSError:
                    pass

        if not config.get("twilio_number"):
            number_key_file = clud_dir / "twilio-from-number.key"
            if number_key_file.exists():
                try:
                    config["twilio_number"] = number_key_file.read_text(encoding="utf-8").strip()
                    sources_used.append("twilio-from-number.key")
                except OSError:
                    pass

    # Priority 4: Legacy JSON file (deprecated, warn user)
    config_file = get_messaging_config_file()
    if config_file.exists() and not config:
        try:
            with open(config_file, encoding="utf-8") as f:
                file_config = json.load(f)

            # Extract nested values
            if "telegram" in file_config and isinstance(file_config["telegram"], dict):
                if not config.get("telegram_token"):
                    token = file_config["telegram"].get("bot_token")
                    if token:
                        config["telegram_token"] = token
                        sources_used.append("messaging.json (DEPRECATED)")

            if "twilio" in file_config and isinstance(file_config["twilio"], dict):
                if not config.get("twilio_sid"):
                    sid = file_config["twilio"].get("account_sid")
                    if sid:
                        config["twilio_sid"] = sid
                        sources_used.append("messaging.json (DEPRECATED)")

                if not config.get("twilio_token"):
                    token = file_config["twilio"].get("auth_token")
                    if token:
                        config["twilio_token"] = token
                        sources_used.append("messaging.json (DEPRECATED)")

                if not config.get("twilio_number"):
                    number = file_config["twilio"].get("from_number")
                    if number:
                        config["twilio_number"] = number
                        sources_used.append("messaging.json (DEPRECATED)")

            # Warn about deprecated storage
            if "messaging.json (DEPRECATED)" in sources_used:
                logger.warning("⚠️  Credentials loaded from plain-text messaging.json (INSECURE)")
                logger.warning("   Run 'clud --configure-messaging' to migrate to encrypted storage")

        except (json.JSONDecodeError, OSError) as e:
            logger.warning(f"Failed to load messaging config from {config_file}: {e}")

    # Remove None values
    config = {k: v for k, v in config.items() if v}

    # Log source for debugging
    if config and sources_used:
        logger.debug(f"Loaded messaging config from: {', '.join(set(sources_used))}")

    return config


def save_messaging_credentials_secure(
    telegram_token: str | None = None,
    twilio_sid: str | None = None,
    twilio_token: str | None = None,
    twilio_number: str | None = None,
) -> bool:
    """Save messaging credentials to secure credential store.

    Uses clud's existing credential infrastructure:
    1. Try system keyring (OS-native)
    2. Try cryptfile keyring (encrypted file)
    3. Fall back to encrypted credential store

    Args:
        telegram_token: Telegram Bot API token
        twilio_sid: Twilio Account SID
        twilio_token: Twilio Auth Token
        twilio_number: Twilio from phone number

    Returns:
        True if saved successfully to credential store, False if fell back to legacy
    """
    try:
        from clud.secrets import get_credential_store

        keyring = get_credential_store()
        if not keyring:
            logger.warning("No credential store available, falling back to legacy JSON storage")
            return False

        # Store in encrypted credential store
        if telegram_token:
            keyring.set_password("clud", "telegram-bot-token", telegram_token)
            logger.info("Saved Telegram token to credential store")

        if twilio_sid:
            keyring.set_password("clud", "twilio-account-sid", twilio_sid)
            logger.info("Saved Twilio SID to credential store")

        if twilio_token:
            keyring.set_password("clud", "twilio-auth-token", twilio_token)
            logger.info("Saved Twilio auth token to credential store")

        if twilio_number:
            keyring.set_password("clud", "twilio-from-number", twilio_number)
            logger.info("Saved Twilio phone number to credential store")

        return True

    except ImportError:
        logger.warning("Credential store modules not available")
        return False
    except Exception as e:
        logger.error(f"Failed to save to credential store: {e}")
        return False


def save_messaging_config(config: dict[str, Any]) -> None:
    """Save messaging configuration to file (LEGACY - use save_messaging_credentials_secure instead).

    This function is kept for backward compatibility but saves to plain-text JSON.
    New code should use save_messaging_credentials_secure() instead.

    Args:
        config: Configuration dictionary with telegram/twilio settings
    """
    logger.warning("Using legacy save_messaging_config() - credentials will be stored in plain text")
    logger.warning("Use save_messaging_credentials_secure() for encrypted storage")

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


def migrate_from_json_to_keyring() -> bool:
    """Migrate credentials from plain JSON to encrypted credential store.

    Returns:
        True if migration successful, False otherwise
    """
    config_file = get_messaging_config_file()
    if not config_file.exists():
        logger.info("No messaging.json to migrate")
        return False

    try:
        # Load from JSON
        with open(config_file, encoding="utf-8") as f:
            data = json.load(f)

        # Extract credentials
        telegram_token = None
        twilio_sid = None
        twilio_token = None
        twilio_number = None

        if "telegram" in data and isinstance(data["telegram"], dict):
            telegram_token = data["telegram"].get("bot_token")

        if "twilio" in data and isinstance(data["twilio"], dict):
            twilio_sid = data["twilio"].get("account_sid")
            twilio_token = data["twilio"].get("auth_token")
            twilio_number = data["twilio"].get("from_number")

        # Save to credential store
        success = save_messaging_credentials_secure(
            telegram_token=telegram_token,
            twilio_sid=twilio_sid,
            twilio_token=twilio_token,
            twilio_number=twilio_number,
        )

        if not success:
            logger.error("Failed to migrate: credential store not available")
            return False

        # Backup old file
        backup = config_file.with_suffix(".json.backup")
        config_file.rename(backup)

        logger.info(f"✓ Migrated credentials from JSON to encrypted credential store")
        logger.info(f"  Old file backed up to: {backup}")
        return True

    except Exception as e:
        logger.error(f"Migration failed: {e}")
        return False


def prompt_for_messaging_config() -> dict[str, Any]:
    """Interactively prompt user for messaging credentials.

    Saves to encrypted credential store (secure) instead of plain JSON.

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

    # Check for existing messaging.json and offer migration
    config_file = get_messaging_config_file()
    if config_file.exists():
        print("\n⚠️  Found existing messaging.json (plain-text storage)")
        sys.stdout.flush()
        migrate = input("Migrate existing credentials to encrypted storage? (Y/n): ").strip().lower()
        if migrate != "n":
            if migrate_from_json_to_keyring():
                print("✓ Existing credentials migrated successfully")

    # Save new config to secure credential store
    try:
        success = save_messaging_credentials_secure(
            telegram_token=config.get("telegram_token"),
            twilio_sid=config.get("twilio_sid"),
            twilio_token=config.get("twilio_token"),
            twilio_number=config.get("twilio_number"),
        )

        if success:
            print("\n✓ Credentials saved securely to encrypted credential store")
            print("  Location: ~/.clud/credentials.enc (encrypted)")
            print("\nYou can now use --notify-user to receive agent status updates!")
        else:
            # Fall back to legacy JSON
            print("\n⚠️  Credential store unavailable, falling back to JSON (less secure)")
            save_messaging_config(config)
            print(f"  Location: {get_messaging_config_file()} (plain text)")
            print("  Consider installing keyring: pip install keyring")

    except Exception as e:
        print(f"\n✗ Failed to save configuration: {e}", file=sys.stderr)

    return config
