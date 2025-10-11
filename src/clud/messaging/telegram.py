"""Telegram messaging implementation for Claude agents."""

# pyright: reportMissingImports=false, reportUnknownMemberType=false, reportUnknownVariableType=false, reportUnknownParameterType=false, reportMissingTypeArgument=false, reportUnknownArgumentType=false, reportMissingParameterType=false

import asyncio
import logging

logger = logging.getLogger(__name__)


class TelegramMessenger:
    """Telegram bot messenger for agent notifications."""

    def __init__(self, bot_token: str, chat_id: str):
        """Initialize Telegram messenger.

        Args:
            bot_token: Telegram bot API token
            chat_id: Telegram chat ID to send messages to
        """
        self.bot_token = bot_token
        self.chat_id = chat_id
        self.bot = None
        self.app = None
        self.message_queue: asyncio.Queue[str] = asyncio.Queue()
        self._initialized = False

    async def _ensure_initialized(self) -> bool:
        """Ensure bot is initialized."""
        if self._initialized:
            return True

        try:
            # Import here to avoid requiring telegram library if not used
            import telegram  # type: ignore[import-untyped]
            from telegram.ext import Application, MessageHandler, filters  # type: ignore[import-untyped]

            self.bot = telegram.Bot(token=self.bot_token)  # type: ignore[attr-defined]
            self.app = Application.builder().token(self.bot_token).build()  # type: ignore[attr-defined]

            # Add message handler
            self.app.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, self._message_handler))  # type: ignore[attr-defined]

            self._initialized = True
            logger.info("Telegram messenger initialized successfully")
            return True
        except ImportError:
            logger.error("python-telegram-bot not installed. Install with: pip install python-telegram-bot")
            return False
        except Exception as e:
            logger.error(f"Failed to initialize Telegram messenger: {e}")
            return False

    async def send_invitation(self, agent_name: str, container_id: str, metadata: dict[str, str]) -> bool:
        """Send invitation message when agent launches.

        Args:
            agent_name: Name of the agent
            container_id: Docker container ID
            metadata: Additional metadata about the agent

        Returns:
            True if message sent successfully, False otherwise
        """
        if not await self._ensure_initialized():
            return False

        try:
            message = f"""
🚀 **Claude Agent Launched**

**Agent**: `{agent_name}`
**Container**: `{container_id[:12]}`
**Project**: {metadata.get("project_path", "N/A")}
**Mode**: {metadata.get("mode", "background")}

Status: ✅ Online and ready

Send messages to interact with your agent!
            """

            await self.bot.send_message(chat_id=self.chat_id, text=message, parse_mode="Markdown")
            logger.info(f"Sent invitation for agent {agent_name}")
            return True
        except Exception as e:
            logger.error(f"Failed to send invitation: {e}")
            return False

    async def send_status_update(self, agent_name: str, status: str, details: dict[str, str] | None = None) -> bool:
        """Send status update during agent operation.

        Args:
            agent_name: Name of the agent
            status: Current status message
            details: Optional additional details

        Returns:
            True if message sent successfully, False otherwise
        """
        if not await self._ensure_initialized():
            return False

        try:
            message = "📊 **Agent Status Update**\n\n"
            message += f"Agent: `{agent_name}`\n"
            message += f"Status: {status}\n"

            if details:
                message += "\n**Details:**\n"
                for key, value in details.items():
                    message += f"- {key}: {value}\n"

            await self.bot.send_message(chat_id=self.chat_id, text=message, parse_mode="Markdown")
            logger.info(f"Sent status update for agent {agent_name}")
            return True
        except Exception as e:
            logger.error(f"Failed to send status update: {e}")
            return False

    async def send_cleanup_notification(self, agent_name: str, summary: dict[str, int | str]) -> bool:
        """Send notification when agent cleans up.

        Args:
            agent_name: Name of the agent
            summary: Summary of agent execution (duration, tasks, etc.)

        Returns:
            True if message sent successfully, False otherwise
        """
        if not await self._ensure_initialized():
            return False

        try:
            message = f"""
✅ **Agent Cleanup Complete**

**Agent**: `{agent_name}`
**Duration**: {summary.get("duration", "N/A")}
**Tasks Completed**: {summary.get("tasks_completed", 0)}
**Files Modified**: {summary.get("files_modified", 0)}
**Errors**: {summary.get("error_count", 0)}

Status: 🔴 Offline
            """

            await self.bot.send_message(chat_id=self.chat_id, text=message, parse_mode="Markdown")
            logger.info(f"Sent cleanup notification for agent {agent_name}")
            return True
        except Exception as e:
            logger.error(f"Failed to send cleanup notification: {e}")
            return False

    async def receive_message(self, timeout: int = 60) -> str | None:
        """Receive message from user.

        Args:
            timeout: Timeout in seconds

        Returns:
            Message text if received, None otherwise
        """
        if not await self._ensure_initialized():
            return None

        try:
            message = await asyncio.wait_for(self.message_queue.get(), timeout=timeout)
            return message
        except asyncio.TimeoutError:
            logger.debug("Message receive timeout")
            return None
        except Exception as e:
            logger.error(f"Failed to receive message: {e}")
            return None

    async def _message_handler(self, update, context):
        """Handle incoming messages from Telegram.

        Args:
            update: Telegram update object
            context: Telegram context object
        """
        if update.message and update.message.text and str(update.message.chat_id) == str(self.chat_id):
            await self.message_queue.put(update.message.text)
            logger.debug(f"Received message: {update.message.text[:50]}")

    async def start_listening(self):
        """Start listening for messages from Telegram."""
        if not await self._ensure_initialized():
            return

        try:
            await self.app.initialize()
            await self.app.start()
            await self.app.updater.start_polling()
            logger.info("Started listening for Telegram messages")
        except Exception as e:
            logger.error(f"Failed to start listening: {e}")

    async def stop_listening(self):
        """Stop listening for messages."""
        if self.app:
            try:
                await self.app.updater.stop()
                await self.app.stop()
                await self.app.shutdown()
                logger.info("Stopped listening for Telegram messages")
            except Exception as e:
                logger.error(f"Failed to stop listening: {e}")
