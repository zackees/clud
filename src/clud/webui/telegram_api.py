"""Telegram API handler for Web UI."""

import asyncio
import logging
from typing import Any

from ..secrets import get_credential_store

logger = logging.getLogger(__name__)

# Service name for credential storage
TELEGRAM_SERVICE = "clud_telegram"
BOT_TOKEN_KEY = "bot_token"
CHAT_ID_KEY = "chat_id"


class TelegramAPIHandler:
    """Handler for Telegram integration API endpoints."""

    def __init__(self) -> None:
        """Initialize Telegram API handler."""
        self.credential_store = get_credential_store()
        if self.credential_store is None:
            logger.warning("No credential store available for Telegram credentials")

    def save_credentials(self, bot_token: str, chat_id: str | None = None) -> bool:
        """Save Telegram credentials.

        Args:
            bot_token: Telegram bot token
            chat_id: Optional chat ID

        Returns:
            True if credentials saved successfully, False otherwise
        """
        if self.credential_store is None:
            logger.error("No credential store available")
            return False

        try:
            # Save bot token
            self.credential_store.set_password(TELEGRAM_SERVICE, BOT_TOKEN_KEY, bot_token)

            # Save chat ID if provided
            if chat_id:
                self.credential_store.set_password(TELEGRAM_SERVICE, CHAT_ID_KEY, chat_id)

            logger.info("Telegram credentials saved successfully")
            return True
        except Exception:
            logger.exception("Failed to save Telegram credentials")
            return False

    def get_credentials(self) -> tuple[str | None, str | None]:
        """Get stored Telegram credentials.

        Returns:
            Tuple of (bot_token, chat_id)
        """
        if self.credential_store is None:
            return None, None

        try:
            bot_token = self.credential_store.get_password(TELEGRAM_SERVICE, BOT_TOKEN_KEY)
            chat_id = self.credential_store.get_password(TELEGRAM_SERVICE, CHAT_ID_KEY)
            return bot_token, chat_id
        except Exception:
            logger.exception("Failed to retrieve Telegram credentials")
            return None, None

    def clear_credentials(self) -> bool:
        """Clear stored Telegram credentials.

        Returns:
            True if credentials cleared successfully, False otherwise
        """
        if self.credential_store is None:
            return False

        try:
            # Set empty strings to clear
            self.credential_store.set_password(TELEGRAM_SERVICE, BOT_TOKEN_KEY, "")
            self.credential_store.set_password(TELEGRAM_SERVICE, CHAT_ID_KEY, "")
            logger.info("Telegram credentials cleared")
            return True
        except Exception:
            logger.exception("Failed to clear Telegram credentials")
            return False

    async def test_bot_connection(self, bot_token: str) -> dict[str, Any] | None:
        """Test Telegram bot connection and get bot info.

        Args:
            bot_token: Telegram bot token to test

        Returns:
            Bot info dict if successful, None otherwise
        """
        try:
            # Import telegram library
            import telegram  # type: ignore[import-untyped]
            from telegram.error import TelegramError  # type: ignore[import-untyped]

            # Create bot instance
            bot = telegram.Bot(token=bot_token)  # type: ignore[attr-defined]

            # Get bot info
            bot_info = await bot.get_me()

            # Return bot information
            return {
                "id": bot_info.id,
                "username": bot_info.username,
                "first_name": bot_info.first_name,
                "deep_link": f"https://t.me/{bot_info.username}",
            }
        except ImportError:
            logger.error("python-telegram-bot not installed - install with: pip install python-telegram-bot")
            return None
        except TelegramError as e:  # type: ignore[misc]
            logger.error(f"Telegram API error: {e}")
            return None
        except Exception as e:
            logger.error(f"Failed to test bot connection: {type(e).__name__}: {e}")
            return None

    def test_bot_connection_sync(self, bot_token: str) -> dict[str, Any] | None:
        """Synchronous wrapper for test_bot_connection.

        Args:
            bot_token: Telegram bot token to test

        Returns:
            Bot info dict if successful, None otherwise
        """
        try:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            result = loop.run_until_complete(self.test_bot_connection(bot_token))
            loop.close()
            return result
        except Exception as e:
            logger.error(f"Error in sync bot test: {e}")
            return None

    async def send_message(self, chat_id: str, message: str) -> bool:
        """Send message to Telegram chat.

        Args:
            chat_id: Telegram chat ID
            message: Message text to send

        Returns:
            True if message sent successfully, False otherwise
        """
        bot_token, _ = self.get_credentials()
        if not bot_token:
            logger.error("No bot token available")
            return False

        try:
            # Import telegram library
            import telegram  # type: ignore[import-untyped]

            # Create bot instance
            bot = telegram.Bot(token=bot_token)  # type: ignore[attr-defined]

            # Send message
            await bot.send_message(chat_id=chat_id, text=message, parse_mode="Markdown")
            return True
        except ImportError:
            logger.error("python-telegram-bot not installed")
            return False
        except Exception as e:
            logger.error(f"Failed to send message: {e}")
            return False

    def is_connected(self) -> bool:
        """Check if Telegram is configured and connected.

        Returns:
            True if bot token is available, False otherwise
        """
        bot_token, _ = self.get_credentials()
        return bot_token is not None and bot_token != ""
