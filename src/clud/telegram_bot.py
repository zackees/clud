"""Telegram bot manager for Claude agents."""

import asyncio
import logging
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# Import Telegram messaging (optional)
try:
    from .messaging import TelegramMessenger

    TELEGRAM_AVAILABLE = True
except ImportError:
    TELEGRAM_AVAILABLE = False
    TelegramMessenger = Any  # type: ignore[misc, assignment]


class TelegramBot:
    """Telegram bot manager for sending agent notifications."""

    def __init__(self, bot_token: str, chat_id: str | None = None, agent_name: str | None = None) -> None:
        """Initialize Telegram bot.

        Args:
            bot_token: Telegram bot API token
            chat_id: Optional Telegram chat ID to send messages to (can be auto-detected)
            agent_name: Optional agent name (will be auto-generated if not provided)
        """
        self.bot_token = bot_token
        self.chat_id = chat_id or ""  # Empty string if not provided
        self.agent_name = agent_name or self._generate_agent_name()
        self.messenger: Any = None
        self.start_time = datetime.now()

    @staticmethod
    def _generate_agent_name() -> str:
        """Generate a default agent name."""
        timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        return f"clud-{timestamp}"

    def _ensure_messenger(self) -> bool:
        """Ensure messenger is initialized."""
        if not TELEGRAM_AVAILABLE:
            print("Warning: Telegram notifications requested but python-telegram-bot is not installed", file=sys.stderr)
            print("Install with: pip install python-telegram-bot", file=sys.stderr)
            return False

        if self.messenger is None:
            try:
                self.messenger = TelegramMessenger(bot_token=self.bot_token, chat_id=self.chat_id)  # type: ignore[misc]
                logger.info("Telegram bot initialized")
            except Exception as e:
                logger.error(f"Failed to create Telegram messenger: {e}")
                return False

        return True

    async def _send_invitation_async(self, metadata: dict[str, str]) -> bool:
        """Send invitation message asynchronously."""
        if not self._ensure_messenger():
            return False

        try:
            success = await self.messenger.send_invitation(agent_name=self.agent_name, container_id=self.agent_name, metadata=metadata)
            if success:
                logger.info("Telegram invitation sent")
            else:
                logger.warning("Failed to send Telegram invitation")
            return success
        except Exception as e:
            logger.error(f"Error sending Telegram invitation: {e}")
            return False

    async def _send_cleanup_async(self, summary: dict[str, int | str]) -> bool:
        """Send cleanup notification asynchronously."""
        if not self._ensure_messenger():
            return False

        try:
            success = await self.messenger.send_cleanup_notification(agent_name=self.agent_name, summary=summary)
            if success:
                logger.info("Telegram cleanup notification sent")
            else:
                logger.warning("Failed to send Telegram cleanup notification")
            return success
        except Exception as e:
            logger.error(f"Error sending Telegram cleanup notification: {e}")
            return False

    def send_invitation(self, project_path: str | Path | None = None, mode: str = "foreground") -> bool:
        """Send invitation message when agent starts.

        Args:
            project_path: Path to the project directory
            mode: Agent mode (foreground/background)

        Returns:
            True if message sent successfully, False otherwise
        """
        metadata = {
            "project_path": str(project_path or Path.cwd()),
            "mode": mode,
            "timestamp": self.start_time.isoformat(),
        }

        try:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            result = loop.run_until_complete(self._send_invitation_async(metadata))
            loop.close()
            return result
        except Exception as e:
            logger.error(f"Error in send_invitation: {e}")
            return False

    def send_cleanup(self, tasks_completed: int = 0, files_modified: int = 0, error_count: int = 0) -> bool:
        """Send cleanup notification when agent ends.

        Args:
            tasks_completed: Number of tasks completed
            files_modified: Number of files modified
            error_count: Number of errors encountered

        Returns:
            True if message sent successfully, False otherwise
        """
        duration = datetime.now() - self.start_time
        duration_str = str(duration).split(".")[0]

        summary = {
            "duration": duration_str,
            "tasks_completed": tasks_completed,
            "files_modified": files_modified,
            "error_count": error_count,
        }

        try:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            result = loop.run_until_complete(self._send_cleanup_async(summary))
            loop.close()
            return result
        except Exception as e:
            logger.error(f"Error in send_cleanup: {e}")
            return False

    @classmethod
    def from_args(cls, args: Any) -> "TelegramBot | None":
        """Create TelegramBot from agent arguments.

        Args:
            args: Agent arguments with telegram fields

        Returns:
            TelegramBot instance or None if disabled
        """
        # Check if telegram is enabled
        telegram_enabled = getattr(args, "telegram", False) or getattr(args, "telegram_enabled", False)
        if not telegram_enabled:
            return None

        # Get credentials
        bot_token = getattr(args, "telegram_bot_token", None)
        chat_id = getattr(args, "telegram_chat_id", None)

        # Only require bot_token - chat_id can be auto-detected from Web App
        if not bot_token:
            print("Error: Telegram notifications require bot token\n", file=sys.stderr)
            print("Quick setup:", file=sys.stderr)
            print("  clud --telegram <bot_token>  (saves token and launches Web App)", file=sys.stderr)
            print('  export TELEGRAM_BOT_TOKEN="..."  (get from @BotFather on Telegram)', file=sys.stderr)
            print("", file=sys.stderr)
            print("Chat ID will be auto-detected when you open the Web App in Telegram", file=sys.stderr)
            print("Setup guide: https://github.com/zackees/clud/blob/main/TELEGRAM_SETUP.md", file=sys.stderr)
            sys.exit(1)

        try:
            return cls(bot_token=bot_token, chat_id=chat_id)
        except Exception as e:
            print(f"Failed to create Telegram bot: {e}", file=sys.stderr)
            return None
