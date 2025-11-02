"""Real Telegram Bot API implementation using python-telegram-bot library.

This module provides a concrete implementation of TelegramBotAPI that wraps
the python-telegram-bot library, enabling actual Telegram bot functionality.
"""

# pyright: reportUnknownMemberType=false, reportUnknownVariableType=false
# pyright: reportUnknownArgumentType=false, reportMissingTypeArgument=false
# Third-party telegram library has incomplete type stubs

import logging
from typing import Any

from telegram import Bot, Update
from telegram.ext import Application, ContextTypes, filters
from telegram.ext import CommandHandler as TgCommandHandler
from telegram.ext import MessageHandler as TgMessageHandler

from clud.telegram.api_interface import (
    CommandHandler,
    ErrorHandler,
    HandlerContext,
    MessageHandler,
    MessageResult,
    TelegramBotAPI,
    TelegramChat,
    TelegramMessage,
    TelegramUpdate,
    TelegramUser,
)

logger = logging.getLogger(__name__)


class RealTelegramBotAPI(TelegramBotAPI):
    """Real implementation of Telegram Bot API using python-telegram-bot library.

    This class wraps the python-telegram-bot library and provides the abstract
    interface defined in TelegramBotAPI, converting between abstract types and
    telegram library types.
    """

    def __init__(self, bot_token: str) -> None:
        """Initialize the real Telegram bot API.

        Args:
            bot_token: Telegram bot API token
        """
        self._bot_token = bot_token
        self._application: Application | None = None
        self._bot: Bot | None = None
        self._command_handlers: dict[str, CommandHandler] = {}
        self._message_handlers: list[MessageHandler] = []
        self._error_handlers: list[ErrorHandler] = []
        self._initialized = False

        logger.info("RealTelegramBotAPI initialized")

    async def initialize(self) -> bool:
        """Initialize the bot API.

        Returns:
            True if initialization was successful, False otherwise.
        """
        if self._initialized:
            return True

        try:
            # Create application and bot instances
            self._application = Application.builder().token(self._bot_token).build()
            self._bot = self._application.bot

            # Register all stored handlers
            for command, handler in self._command_handlers.items():
                wrapper = self._create_command_wrapper(handler)
                self._application.add_handler(TgCommandHandler(command, wrapper))

            for handler in self._message_handlers:
                wrapper = self._create_message_wrapper(handler)
                self._application.add_handler(TgMessageHandler(filters.TEXT & ~filters.COMMAND, wrapper))

            for handler in self._error_handlers:
                wrapper = self._create_error_wrapper(handler)
                self._application.add_error_handler(wrapper)

            # Initialize the application
            await self._application.initialize()

            self._initialized = True
            logger.info("Telegram bot initialized successfully")
            return True

        except Exception as e:
            logger.error(f"Failed to initialize Telegram bot: {e}")
            return False

    async def shutdown(self) -> None:
        """Shutdown the bot API gracefully."""
        if not self._application:
            return

        try:
            logger.info("Shutting down Telegram bot...")

            # Stop polling if it's running
            if self._application.updater and self._application.updater.running:
                await self._application.updater.stop()

            # Stop and shutdown the application
            if self._application.running:
                await self._application.stop()

            await self._application.shutdown()

            self._initialized = False
            logger.info("Telegram bot shutdown complete")

        except Exception as e:
            logger.error(f"Error during Telegram bot shutdown: {e}")

    async def send_message(
        self,
        chat_id: str | int,
        text: str,
        parse_mode: str | None = None,
        reply_to_message_id: int | None = None,
    ) -> MessageResult:
        """Send a text message to a chat.

        Args:
            chat_id: Telegram chat ID
            text: Message text to send
            parse_mode: Optional parse mode (e.g., "Markdown", "HTML")
            reply_to_message_id: Optional message ID to reply to

        Returns:
            MessageResult indicating success/failure and message ID
        """
        if not self._bot:
            return MessageResult(success=False, error="Bot not initialized")

        try:
            # Convert chat_id to appropriate type
            chat_id_value = int(chat_id) if isinstance(chat_id, str) else chat_id

            # Send the message
            message = await self._bot.send_message(
                chat_id=chat_id_value,
                text=text,
                parse_mode=parse_mode,
                reply_to_message_id=reply_to_message_id,
            )

            return MessageResult(
                success=True,
                message_id=message.message_id,
            )

        except Exception as e:
            logger.error(f"Failed to send message to chat {chat_id}: {e}")
            return MessageResult(
                success=False,
                error=str(e),
            )

    async def send_typing_action(self, chat_id: str | int) -> bool:
        """Send typing action to a chat.

        Args:
            chat_id: Telegram chat ID

        Returns:
            True if action was sent successfully, False otherwise
        """
        if not self._bot:
            logger.warning("Bot not initialized, cannot send typing action")
            return False

        try:
            # Convert chat_id to appropriate type
            chat_id_value = int(chat_id) if isinstance(chat_id, str) else chat_id

            await self._bot.send_chat_action(chat_id=chat_id_value, action="typing")
            return True

        except Exception as e:
            logger.error(f"Failed to send typing action to chat {chat_id}: {e}")
            return False

    async def start_polling(self, drop_pending_updates: bool = True) -> None:
        """Start polling for updates.

        Args:
            drop_pending_updates: Whether to drop pending updates on start
        """
        if not self._application:
            raise RuntimeError("Bot not initialized. Call initialize() first.")

        try:
            # Start the application if not already running
            if not self._application.running:
                await self._application.start()

            # Start polling
            await self._application.updater.start_polling(drop_pending_updates=drop_pending_updates)

            logger.info("Telegram bot polling started")

        except Exception as e:
            logger.error(f"Failed to start polling: {e}")
            raise RuntimeError(f"Failed to start polling: {e}") from e

    async def stop_polling(self) -> None:
        """Stop polling for updates."""
        if not self._application or not self._application.updater:
            return

        try:
            if self._application.updater.running:
                await self._application.updater.stop()
                logger.info("Telegram bot polling stopped")

        except Exception as e:
            logger.error(f"Error stopping polling: {e}")

    def add_command_handler(self, command: str, handler: CommandHandler) -> None:
        """Add a command handler.

        Args:
            command: Command name (without leading slash)
            handler: Async function to handle the command
        """
        # Store the handler
        self._command_handlers[command] = handler

        # If already initialized, add to application
        if self._initialized and self._application:
            wrapper = self._create_command_wrapper(handler)
            self._application.add_handler(TgCommandHandler(command, wrapper))
            logger.debug(f"Added command handler for /{command}")

    def add_message_handler(self, handler: MessageHandler) -> None:
        """Add a text message handler.

        Args:
            handler: Async function to handle text messages
        """
        # Store the handler
        self._message_handlers.append(handler)

        # If already initialized, add to application
        if self._initialized and self._application:
            wrapper = self._create_message_wrapper(handler)
            self._application.add_handler(TgMessageHandler(filters.TEXT & ~filters.COMMAND, wrapper))
            logger.debug("Added message handler")

    def add_error_handler(self, handler: ErrorHandler) -> None:
        """Add an error handler.

        Args:
            handler: Async function to handle errors
        """
        # Store the handler
        self._error_handlers.append(handler)

        # If already initialized, add to application
        if self._initialized and self._application:
            wrapper = self._create_error_wrapper(handler)
            self._application.add_error_handler(wrapper)
            logger.debug("Added error handler")

    async def get_me(self) -> TelegramUser | None:
        """Get information about the bot.

        Returns:
            TelegramUser object representing the bot, or None if failed
        """
        if not self._bot:
            logger.warning("Bot not initialized, cannot get bot info")
            return None

        try:
            bot_info = await self._bot.get_me()
            return self._convert_user(bot_info)

        except Exception as e:
            logger.error(f"Failed to get bot info: {e}")
            return None

    # Type conversion methods

    @staticmethod
    def _convert_user(user: Any) -> TelegramUser:
        """Convert telegram.User to TelegramUser.

        Args:
            user: telegram.User object

        Returns:
            TelegramUser abstraction
        """
        return TelegramUser(
            id=user.id,
            username=user.username,
            first_name=user.first_name,
            last_name=user.last_name,
            is_bot=user.is_bot,
        )

    @staticmethod
    def _convert_chat(chat: Any) -> TelegramChat:
        """Convert telegram.Chat to TelegramChat.

        Args:
            chat: telegram.Chat object

        Returns:
            TelegramChat abstraction
        """
        return TelegramChat(
            id=chat.id,
            type=chat.type,
            title=chat.title,
            username=chat.username,
        )

    @staticmethod
    def _convert_message(message: Any) -> TelegramMessage:
        """Convert telegram.Message to TelegramMessage.

        Args:
            message: telegram.Message object

        Returns:
            TelegramMessage abstraction
        """
        return TelegramMessage(
            message_id=message.message_id,
            chat=RealTelegramBotAPI._convert_chat(message.chat),
            from_user=RealTelegramBotAPI._convert_user(message.from_user) if message.from_user else None,
            text=message.text,
            date=message.date,
        )

    @staticmethod
    def _convert_update(update: Update) -> TelegramUpdate:
        """Convert telegram.Update to TelegramUpdate.

        Args:
            update: telegram.Update object

        Returns:
            TelegramUpdate abstraction
        """
        return TelegramUpdate(
            update_id=update.update_id,
            message=RealTelegramBotAPI._convert_message(update.message) if update.message else None,
            effective_user=RealTelegramBotAPI._convert_user(update.effective_user) if update.effective_user else None,
            effective_chat=RealTelegramBotAPI._convert_chat(update.effective_chat) if update.effective_chat else None,
        )

    # Handler wrapper methods

    def _create_command_wrapper(self, handler: CommandHandler) -> Any:  # Returns telegram.ext handler function
        """Create a wrapper that converts telegram types to abstract types.

        Args:
            handler: Abstract command handler

        Returns:
            Telegram library command handler function
        """

        async def wrapper(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
            try:
                abstract_update = self._convert_update(update)
                handler_context = HandlerContext(
                    bot=self,
                    user_data=context.user_data,
                    chat_data=context.chat_data,
                )
                await handler(abstract_update, handler_context)
            except Exception as e:
                logger.error(f"Error in command handler: {e}", exc_info=True)

        return wrapper

    def _create_message_wrapper(self, handler: MessageHandler) -> Any:  # Returns telegram.ext handler function
        """Create a wrapper that converts telegram types to abstract types.

        Args:
            handler: Abstract message handler

        Returns:
            Telegram library message handler function
        """

        async def wrapper(update: Update, context: ContextTypes.DEFAULT_TYPE) -> None:
            try:
                abstract_update = self._convert_update(update)
                handler_context = HandlerContext(
                    bot=self,
                    user_data=context.user_data,
                    chat_data=context.chat_data,
                )
                await handler(abstract_update, handler_context)
            except Exception as e:
                logger.error(f"Error in message handler: {e}", exc_info=True)

        return wrapper

    def _create_error_wrapper(self, handler: ErrorHandler) -> Any:  # Returns telegram.ext error handler function
        """Create a wrapper that converts telegram types to abstract types.

        Args:
            handler: Abstract error handler

        Returns:
            Telegram library error handler function
        """

        async def wrapper(update: object, context: ContextTypes.DEFAULT_TYPE) -> None:
            try:
                abstract_update = None
                if isinstance(update, Update):
                    abstract_update = self._convert_update(update)

                handler_context = HandlerContext(
                    bot=self,
                    user_data=context.user_data if hasattr(context, "user_data") else None,
                    chat_data=context.chat_data if hasattr(context, "chat_data") else None,
                    error=context.error,
                )
                await handler(abstract_update, handler_context)
            except Exception as e:
                logger.error(f"Error in error handler: {e}", exc_info=True)

        return wrapper


__all__ = ["RealTelegramBotAPI"]
