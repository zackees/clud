"""
Configuration directory and file management.

This module handles configuration file I/O operations, including
directory creation and credential storage.
"""

import sys
from pathlib import Path

from clud.agent.exceptions import ConfigError
from clud.secrets import get_credential_store

# Get credential store once at module level
keyring = get_credential_store()


def get_clud_config_dir() -> Path:
    """Get or create the .clud config directory."""
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(exist_ok=True)
    return config_dir


def save_telegram_credentials(bot_token: str, chat_id: str) -> None:
    """Save Telegram credentials using the credential store.

    Args:
        bot_token: Telegram bot token (required)
        chat_id: Telegram chat ID (can be empty string if not yet known)
    """
    if keyring is None:
        raise ConfigError("No credential storage available. Install with: pip install keyring, keyrings.cryptfile, or cryptography")

    try:
        # Save bot token (always required)
        keyring.set_password("clud-telegram", "bot-token", bot_token.strip())

        # Save chat_id only if it's not empty
        if chat_id and chat_id.strip():
            keyring.set_password("clud-telegram", "chat-id", chat_id.strip())
    except Exception as e:
        raise ConfigError(f"Failed to save Telegram credentials: {e}") from e


def load_telegram_credentials() -> tuple[str | None, str | None]:
    """Load Telegram credentials from credential store.

    Returns:
        Tuple of (bot_token, chat_id) or (None, None) if not found
    """
    if keyring is None:
        return None, None

    try:
        bot_token = keyring.get_password("clud-telegram", "bot-token")
        chat_id = keyring.get_password("clud-telegram", "chat-id")
        return bot_token, chat_id
    except Exception as e:
        print(f"Warning: Could not load Telegram credentials: {e}", file=sys.stderr)
        return None, None
