"""Telegram Bot API client implementation."""

import asyncio
import logging
from typing import Any

logger = logging.getLogger(__name__)


class TelegramClient:
    """Telegram Bot API client using python-telegram-bot.

    Note: This class is defined but requires python-telegram-bot to be installed.
    Import errors are handled gracefully at the factory level.
    """

    def __init__(self, token: str) -> None:
        """Initialize Telegram client.

        Args:
            token: Telegram Bot API token from @BotFather
        """
        try:
            from telegram import Bot
            from telegram.error import TelegramError

            self.Bot = Bot
            self.TelegramError = TelegramError
            self.bot = Bot(token=token)
            self._chat_id_cache: dict[str, int] = {}
            self._available = True
        except ImportError:
            logger.warning("python-telegram-bot not installed. Install with: pip install python-telegram-bot")
            self._available = False

    async def send_message(self, contact: str, message: str) -> bool:
        """Send message via Telegram.

        Args:
            contact: @username or chat_id (numeric)
            message: Text to send (supports Markdown)

        Returns:
            True if sent successfully
        """
        if not self._available:
            logger.error("Telegram client not available (missing python-telegram-bot)")
            return False

        try:
            chat_id = await self._resolve_chat_id(contact)
            await self.bot.send_message(chat_id=chat_id, text=message, parse_mode="Markdown")
            logger.info(f"Telegram message sent to {contact}")
            return True
        except self.TelegramError as e:
            logger.error(f"Telegram send failed to {contact}: {e}")
            return False
        except Exception as e:
            logger.error(f"Unexpected error sending Telegram message: {e}")
            return False

    async def send_code_block(self, contact: str, code: str, language: str = "python") -> bool:
        """Send formatted code block.

        Args:
            contact: @username or chat_id
            code: Code content
            language: Programming language for syntax highlighting

        Returns:
            True if sent successfully
        """
        # Telegram supports code blocks with backticks
        formatted = f"```{language}\n{code}\n```"
        return await self.send_message(contact, formatted)

    async def _resolve_chat_id(self, contact: str) -> int:
        """Convert contact string to Telegram chat_id.

        Args:
            contact: Can be @username, numeric chat_id, or telegram:@username

        Returns:
            Numeric chat_id

        Raises:
            ValueError: If contact format is invalid
        """
        # Strip telegram: prefix if present
        if contact.startswith("telegram:"):
            contact = contact[9:]

        # Check cache first
        if contact in self._chat_id_cache:
            return self._chat_id_cache[contact]

        # If already numeric, return as-is
        if contact.lstrip("-").isdigit():
            chat_id = int(contact)
            self._chat_id_cache[contact] = chat_id
            return chat_id

        # For @username, we need the user to have started a conversation with the bot first
        # This is a limitation of Telegram Bot API - we can't look up users by username
        # The user must send /start to the bot, then we can message them
        # For now, raise an error with helpful message
        raise ValueError(
            f"Cannot resolve Telegram username '{contact}' to chat_id. "
            "User must send /start to the bot first, then use their numeric chat_id. "
            "Get chat_id by sending a message to the bot and checking bot.get_updates()"
        )

    def is_available(self) -> bool:
        """Check if Telegram client is available."""
        return self._available
