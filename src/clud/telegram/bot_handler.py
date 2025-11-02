"""Telegram bot handler for receiving and responding to messages.

This module provides the TelegramBotHandler class which handles incoming messages
from Telegram users, routes them to the SessionManager, and sends responses back.
"""

import logging

from clud.telegram.api_interface import HandlerContext, TelegramBotAPI, TelegramUpdate
from clud.telegram.config import TelegramIntegrationConfig
from clud.telegram.session_manager import SessionManager

logger = logging.getLogger(__name__)


class TelegramBotHandler:
    """Handles Telegram bot interactions.

    Receives messages from Telegram users via webhook or polling, authenticates users,
    routes messages to SessionManager, and sends responses back to Telegram.
    """

    def __init__(
        self,
        config: TelegramIntegrationConfig,
        session_manager: SessionManager,
        api: TelegramBotAPI,
    ) -> None:
        """Initialize the bot handler.

        Args:
            config: Telegram integration configuration
            session_manager: Session manager for handling conversations
            api: Telegram Bot API implementation
        """
        self.config = config
        self.session_manager = session_manager
        self.api = api

        logger.info(f"TelegramBotHandler initialized for bot with polling={config.telegram.polling}")

    async def start_command(self, update: TelegramUpdate, context: HandlerContext) -> None:
        """Handle /start command.

        Args:
            update: Telegram update object
            context: Handler context
        """
        if not update.effective_user or not update.message:
            return

        user_id = update.effective_user.id
        username = update.effective_user.username or f"user_{user_id}"

        # Check if user is allowed
        if not self._is_user_allowed(user_id):
            await self.api.send_message(
                chat_id=update.message.chat.id,
                text="Sorry, you are not authorized to use this bot. Please contact the administrator.",
            )
            logger.warning(f"Unauthorized user {user_id} (@{username}) attempted to use bot")
            return

        # Create or get session
        try:
            session = await self.session_manager.get_or_create_session(
                telegram_user_id=user_id,
                telegram_username=username,
                telegram_first_name=update.effective_user.first_name or "",
                telegram_last_name=update.effective_user.last_name,
            )

            welcome_message = (
                f"👋 Welcome to Claude Code, {update.effective_user.first_name}!\n\n"
                "I'm your AI coding assistant powered by Claude. I can help you with:\n"
                "• Writing and debugging code\n"
                "• Explaining technical concepts\n"
                "• Reviewing and refactoring code\n"
                "• Answering programming questions\n\n"
                "Just send me a message and I'll help you out!\n\n"
                "Commands:\n"
                "/help - Show this help message\n"
                "/clear - Clear conversation history\n"
                "/status - Show session status"
            )

            await self.api.send_message(chat_id=update.message.chat.id, text=welcome_message)
            logger.info(f"Started session {session.session_id} for user {user_id} (@{username})")

        except Exception as e:
            logger.error(f"Failed to create session for user {user_id}: {e}")
            await self.api.send_message(
                chat_id=update.message.chat.id,
                text="Sorry, an error occurred while starting your session. Please try again later.",
            )

    async def help_command(self, update: TelegramUpdate, context: HandlerContext) -> None:
        """Handle /help command.

        Args:
            update: Telegram update object
            context: Handler context
        """
        if not update.message:
            return

        help_text = (
            "🤖 *Claude Code Bot Help*\n\n"
            "*Available Commands:*\n"
            "/start - Start a new session\n"
            "/help - Show this help message\n"
            "/clear - Clear conversation history\n"
            "/status - Show session status\n\n"
            "*How to Use:*\n"
            "Simply send me a message with your coding question or task, and I'll assist you!\n\n"
            "*Features:*\n"
            "• Natural language interaction\n"
            "• Code generation and debugging\n"
            "• Technical explanations\n"
            "• Code review and refactoring\n\n"
            "*Web Dashboard:*\n"
            f"Monitor your conversations at: http://{self.config.web.host}:{self.config.web.port}/telegram"
        )

        await self.api.send_message(chat_id=update.message.chat.id, text=help_text, parse_mode="Markdown")

    async def status_command(self, update: TelegramUpdate, context: HandlerContext) -> None:
        """Handle /status command.

        Args:
            update: Telegram update object
            context: Handler context
        """
        if not update.effective_user or not update.message:
            return

        user_id = update.effective_user.id
        session = self.session_manager.get_user_session(user_id)

        if not session:
            await self.api.send_message(
                chat_id=update.message.chat.id,
                text="No active session found. Use /start to begin.",
            )
            return

        # Calculate session stats
        message_count = len(session.message_history)
        user_messages = sum(1 for msg in session.message_history if msg.sender.value == "user")
        bot_messages = sum(1 for msg in session.message_history if msg.sender.value == "bot")

        # Format uptime
        uptime = (session.last_activity - session.created_at).total_seconds()
        uptime_str = self._format_duration(uptime)

        status_text = (
            f"📊 *Session Status*\n\n"
            f"Session ID: `{session.session_id[:8]}...`\n"
            f"User: {session.get_display_name()} (@{session.telegram_username})\n"
            f"Uptime: {uptime_str}\n"
            f"Messages: {message_count} total ({user_messages} from you, {bot_messages} from bot)\n"
            f"Web Clients: {session.web_client_count} connected\n"
            f"Active: {'✅ Yes' if session.is_active else '❌ No'}"
        )

        await self.api.send_message(chat_id=update.message.chat.id, text=status_text, parse_mode="Markdown")

    async def clear_command(self, update: TelegramUpdate, context: HandlerContext) -> None:
        """Handle /clear command.

        Args:
            update: Telegram update object
            context: Handler context
        """
        if not update.effective_user or not update.message:
            return

        user_id = update.effective_user.id
        session = self.session_manager.get_user_session(user_id)

        if not session:
            await self.api.send_message(
                chat_id=update.message.chat.id,
                text="No active session found. Use /start to begin.",
            )
            return

        # Clear message history
        session.message_history.clear()
        await self.api.send_message(chat_id=update.message.chat.id, text="✅ Conversation history cleared!")
        logger.info(f"Cleared history for session {session.session_id}")

    async def handle_message(self, update: TelegramUpdate, context: HandlerContext) -> None:
        """Handle regular text messages.

        Args:
            update: Telegram update object
            context: Handler context
        """
        if not update.effective_user or not update.message or not update.message.text:
            return

        user_id = update.effective_user.id
        username = update.effective_user.username or f"user_{user_id}"
        message_text = update.message.text

        # Check if user is allowed
        if not self._is_user_allowed(user_id):
            await self.api.send_message(
                chat_id=update.message.chat.id,
                text="Sorry, you are not authorized to use this bot. Please contact the administrator.",
            )
            logger.warning(f"Unauthorized user {user_id} (@{username}) attempted to send message")
            return

        # Get or create session
        try:
            session = self.session_manager.get_user_session(user_id)
            if not session:
                session = await self.session_manager.get_or_create_session(
                    telegram_user_id=user_id,
                    telegram_username=username,
                    telegram_first_name=update.effective_user.first_name or "",
                    telegram_last_name=update.effective_user.last_name,
                )

            # Send typing indicator
            await self.api.send_typing_action(chat_id=update.message.chat.id)

            # Process message through clud
            response = await self.session_manager.process_user_message(
                session_id=session.session_id,
                message_content=message_text,
                telegram_message_id=update.message.message_id,
            )

            # Send response back to user
            # Note: send_message in RealTelegramBotAPI handles message splitting automatically
            await self.api.send_message(chat_id=update.message.chat.id, text=response)

            logger.info(f"Processed message in session {session.session_id}: {message_text[:50]}...")

        except Exception as e:
            logger.error(f"Error handling message for user {user_id}: {e}")
            await self.api.send_message(
                chat_id=update.message.chat.id,
                text="Sorry, an error occurred while processing your message. Please try again.",
            )

    async def error_handler(self, update: TelegramUpdate | None, context: HandlerContext) -> None:
        """Handle errors.

        Args:
            update: Telegram update object (may be None)
            context: Handler context
        """
        logger.error(f"Update {update} caused error: {context.error}", exc_info=context.error)

    def _is_user_allowed(self, user_id: int) -> bool:
        """Check if a user is allowed to use the bot.

        Args:
            user_id: Telegram user ID

        Returns:
            True if user is allowed, False otherwise
        """
        # If no whitelist configured, allow all users
        if not self.config.telegram.allowed_users:
            return True

        return user_id in self.config.telegram.allowed_users

    def _format_duration(self, seconds: float) -> str:
        """Format duration in seconds to human-readable string.

        Args:
            seconds: Duration in seconds

        Returns:
            Formatted duration string
        """
        if seconds < 60:
            return f"{int(seconds)}s"
        elif seconds < 3600:
            minutes = int(seconds / 60)
            return f"{minutes}m"
        else:
            hours = int(seconds / 3600)
            minutes = int((seconds % 3600) / 60)
            return f"{hours}h {minutes}m"

    async def start_polling(self) -> None:
        """Start the bot with polling mode.

        Raises:
            RuntimeError: If bot fails to start
        """
        try:
            # Initialize the API
            if not await self.api.initialize():
                raise RuntimeError("Failed to initialize Telegram bot API")

            # Register handlers with the API
            self.api.add_command_handler("start", self.start_command)
            self.api.add_command_handler("help", self.help_command)
            self.api.add_command_handler("status", self.status_command)
            self.api.add_command_handler("clear", self.clear_command)
            self.api.add_message_handler(self.handle_message)
            self.api.add_error_handler(self.error_handler)

            # Start polling
            await self.api.start_polling(drop_pending_updates=True)

            logger.info("Telegram bot started in polling mode")

        except Exception as e:
            logger.error(f"Failed to start Telegram bot: {e}")
            raise RuntimeError(f"Failed to start Telegram bot: {e}") from e

    async def stop(self) -> None:
        """Stop the bot gracefully."""
        try:
            logger.info("Stopping Telegram bot...")
            await self.api.stop_polling()
            await self.api.shutdown()
            logger.info("Telegram bot stopped")
        except Exception as e:
            logger.error(f"Error stopping Telegram bot: {e}")
