"""High-level notification manager for agent status updates."""

import asyncio
import logging
import time
from typing import Any

from .telegram_client import TelegramClient
from .twilio_client import TwilioClient

logger = logging.getLogger(__name__)


class AgentNotifier:
    """High-level notification manager for agent status updates.

    Handles periodic status updates with rate limiting and error handling.
    """

    def __init__(
        self,
        client: TelegramClient | TwilioClient,
        contact: str,
        update_interval: int = 30,
    ) -> None:
        """Initialize notifier.

        Args:
            client: Messaging client (Telegram or Twilio)
            contact: User contact identifier
            update_interval: Seconds between progress updates (default: 30)
        """
        self.client = client
        self.contact = contact
        self.update_interval = update_interval
        self._last_update_time = 0.0
        self._start_time = 0.0

    async def notify_start(self, task: str) -> None:
        """Notify user that agent is starting.

        Args:
            task: Task description
        """
        self._start_time = time.time()
        message = f"🤖 **Clud Agent Starting**\n\nTask: {task}\n\nI'll keep you updated on progress!"
        success = await self.client.send_message(self.contact, message)
        if not success:
            logger.warning("Failed to send start notification")

    async def notify_progress(self, status: str) -> None:
        """Send periodic progress updates (respects update_interval).

        Args:
            status: Current status message
        """
        current_time = time.time()

        # Rate limit: don't send more frequently than update_interval
        if current_time - self._last_update_time < self.update_interval:
            return

        elapsed = int(current_time - self._start_time)
        message = f"⏳ **Working** ({elapsed}s)\n\n{status}"

        success = await self.client.send_message(self.contact, message)
        if success:
            self._last_update_time = current_time
        else:
            logger.warning("Failed to send progress notification")

    async def notify_completion(self, success: bool, summary: str = "") -> None:
        """Notify user of completion.

        Args:
            success: Whether task completed successfully
            summary: Optional summary of results
        """
        duration = int(time.time() - self._start_time)
        emoji = "✅" if success else "❌"
        status = "Completed Successfully" if success else "Failed"

        message = f"{emoji} **{status}** ({duration}s)"
        if summary:
            message += f"\n\n{summary}"

        await self.client.send_message(self.contact, message)

    async def notify_error(self, error: str) -> None:
        """Notify user of error.

        Args:
            error: Error message
        """
        # Truncate very long errors
        if len(error) > 500:
            error = error[:500] + "...\n[truncated]"

        message = f"⚠️ **Error**\n\n```\n{error}\n```"
        await self.client.send_message(self.contact, message)

    async def notify_status(self, emoji: str, title: str, body: str = "") -> None:
        """Send custom status notification.

        Args:
            emoji: Emoji for status
            title: Status title
            body: Optional body text
        """
        message = f"{emoji} **{title}**"
        if body:
            message += f"\n\n{body}"

        await self.client.send_message(self.contact, message)

    def should_send_progress_update(self) -> bool:
        """Check if enough time has passed to send progress update.

        Returns:
            True if update should be sent
        """
        current_time = time.time()
        return current_time - self._last_update_time >= self.update_interval
