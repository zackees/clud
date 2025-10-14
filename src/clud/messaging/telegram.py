"""Telegram messaging implementation for Claude agents."""

# pyright: reportMissingImports=false, reportUnknownMemberType=false, reportUnknownVariableType=false, reportUnknownParameterType=false, reportMissingTypeArgument=false, reportUnknownArgumentType=false, reportMissingParameterType=false

import asyncio
import logging
from typing import Any

logger = logging.getLogger(__name__)


class TelegramMessenger:
    """Telegram bot messenger for agent notifications."""

    def __init__(self, bot_token: str, chat_id: str) -> None:
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

    async def send_invitation(self, agent_name: str, process_id: str, metadata: dict[str, str]) -> bool:
        """Send invitation message when agent launches.

        Args:
            agent_name: Name of the agent
            process_id: Process ID
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
**Process**: `{process_id}`
**Project**: {metadata.get("project_path", "N/A")}
**Mode**: {metadata.get("mode", "foreground")}

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
        except TimeoutError:
            logger.debug("Message receive timeout")
            return None
        except Exception as e:
            logger.error(f"Failed to receive message: {e}")
            return None

    async def _message_handler(self, update: Any, context: Any) -> None:
        """Handle incoming messages from Telegram.

        Args:
            update: Telegram update object
            context: Telegram context object
        """
        if update.message and update.message.text and str(update.message.chat_id) == str(self.chat_id):
            await self.message_queue.put(update.message.text)
            logger.debug(f"Received message: {update.message.text[:50]}")

    async def start_listening(self) -> None:
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

    async def stop_listening(self) -> None:
        """Stop listening for messages."""
        if self.app:
            try:
                await self.app.updater.stop()
                await self.app.stop()
                await self.app.shutdown()
                logger.info("Stopped listening for Telegram messages")
            except Exception as e:
                logger.error(f"Failed to stop listening: {e}")

    async def handle_web_app_data(self, update: Any, context: Any, message_handler: Any = None) -> bool:
        """Handle data sent from Telegram Web App.

        Args:
            update: Telegram update with web_app_data
            context: Telegram context
            message_handler: Optional MessageHandler instance for processing messages

        Returns:
            True if message handled successfully, False otherwise
        """
        if not update.message or not update.message.web_app_data:
            logger.warning("No web app data in update")
            return False

        # Extract chat context for multi-chat support
        chat_id = str(update.message.chat_id)
        user_id = update.message.from_user.id
        username = update.message.from_user.username or "Unknown"

        # Import json at function scope
        import json

        try:
            # Parse the JSON data sent from web app
            data = json.loads(update.message.web_app_data.data)

            message_text = data.get("text", "")
            message_type = data.get("type", "message")

            logger.info(f"Web app {message_type} from {username} (user_id: {user_id}, chat_id: {chat_id}): {message_text[:50]}...")

            # If MessageHandler is available, use it to process the message
            if message_handler:
                from clud.api.models import ClientType, MessageRequest

                # Create MessageRequest from Telegram update
                request = MessageRequest(
                    message=message_text,
                    session_id=chat_id,  # Use chat_id as session_id for persistence
                    client_type=ClientType.TELEGRAM,
                    client_id=str(user_id),
                    metadata={"username": username, "message_type": message_type},
                )

                # Forward to MessageHandler API
                response = await message_handler.handle_message(request)

                # Send response back to Telegram
                if response.error:
                    await update.message.reply_text(f"❌ Error: {response.error}", parse_mode="Markdown")
                else:
                    status_emoji = {"completed": "✅", "running": "⏳", "failed": "❌"}.get(response.status.value, "📝")
                    reply = f"{status_emoji} Message processed\nInstance: `{response.instance_id[:8]}...`"
                    if response.message:
                        reply += f"\n\n{response.message}"
                    await update.message.reply_text(reply, parse_mode="Markdown")

                return True
            else:
                # Fallback: just echo back the message
                response = f"Received your message: {message_text}\n\n(Agent integration coming soon!)"
                await update.message.reply_text(response, parse_mode="Markdown")
                return True

        except json.JSONDecodeError as e:
            logger.error(f"Invalid JSON from web app (chat {chat_id}): {e}")
            await update.message.reply_text("Sorry, I received invalid data. Please try again.")
            return False
        except Exception as e:
            logger.error(f"Error processing web app data (chat {chat_id}): {e}")
            await update.message.reply_text("Sorry, an error occurred. Please try again.")
            return False

    async def setup_web_app_handler(self) -> None:
        """Set up handler for web app data."""
        if not await self._ensure_initialized():
            return

        try:
            # Import here to avoid requiring telegram library if not used
            from telegram import filters  # type: ignore[import-untyped]
            from telegram.ext import MessageHandler  # type: ignore[import-untyped]

            # Add handler for web app data
            web_app_handler = MessageHandler(filters.StatusUpdate.WEB_APP_DATA, self.handle_web_app_data)  # type: ignore[attr-defined]
            self.app.add_handler(web_app_handler)  # type: ignore[attr-defined]

            logger.info("Web app handler registered")
        except Exception as e:
            logger.error(f"Failed to setup web app handler: {e}")
