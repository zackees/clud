"""Telegram hook handler for streaming output to Telegram chats.

This module provides a hook handler that sends execution events
to Telegram chats, enabling real-time output streaming.
"""

import asyncio
import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from datetime import datetime

from clud.hooks import HookContext, HookEvent

logger = logging.getLogger(__name__)

# Maximum message length for Telegram (subtract some for formatting)
TELEGRAM_MAX_MESSAGE_LENGTH = 2000


@dataclass
class TelegramMessage:
    """Represents a buffered Telegram message.

    Attributes:
        text: The message text content
        chat_id: Telegram chat ID to send to
        created_at: When the message was created
    """

    text: str
    chat_id: str
    created_at: datetime = field(default_factory=datetime.now)


class TelegramHookHandler:
    """Hook handler that sends events to Telegram chats.

    This handler buffers output chunks and sends them to Telegram
    when the buffer reaches a certain size or after a timeout.
    """

    def __init__(
        self,
        bot_token: str,
        send_callback: Callable[[str, str], Awaitable[None]] | None = None,
        buffer_size: int = TELEGRAM_MAX_MESSAGE_LENGTH,
        flush_interval: float = 2.0,
    ) -> None:
        """Initialize the Telegram hook handler.

        Args:
            bot_token: Telegram bot API token
            send_callback: Optional async callback function to send messages
                          Should accept (chat_id: str, text: str) and return None
            buffer_size: Maximum buffer size before auto-flush (default: 2000)
            flush_interval: Time in seconds to wait before auto-flush (default: 2.0)
        """
        self._bot_token = bot_token
        self._send_callback = send_callback
        self._buffer_size = buffer_size
        self._flush_interval = flush_interval

        # Buffer management: session_id -> list of text chunks
        self._buffers: dict[str, list[str]] = {}
        self._buffer_sizes: dict[str, int] = {}
        self._flush_tasks: dict[str, asyncio.Task[None]] = {}

    async def handle(self, context: HookContext) -> None:
        """Handle a hook event and forward to Telegram.

        Args:
            context: The hook context containing event information
        """
        if context.client_type != "telegram":
            # Only handle Telegram client events
            return

        try:
            if context.event == HookEvent.AGENT_START:
                await self._handle_agent_start(context)
            elif context.event == HookEvent.OUTPUT_CHUNK:
                await self._handle_output_chunk(context)
            elif context.event == HookEvent.POST_EXECUTION:
                await self._handle_post_execution(context)
            elif context.event == HookEvent.ERROR:
                await self._handle_error(context)
            elif context.event == HookEvent.AGENT_STOP:
                await self._handle_agent_stop(context)

        except Exception as e:
            logger.error(f"Error handling Telegram hook event {context.event.value}: {e}", exc_info=True)

    async def _handle_agent_start(self, context: HookContext) -> None:
        """Handle agent start event.

        Args:
            context: The hook context
        """
        message = f"🚀 *Agent Started*\n\nSession: `{context.session_id}`\nInstance: `{context.instance_id[:8]}...`\nType: {context.client_type}\n\nReady to process your commands!"
        await self._send_message(context.session_id, message)

    async def _handle_output_chunk(self, context: HookContext) -> None:
        """Handle output chunk event by buffering the output.

        Args:
            context: The hook context
        """
        if not context.output:
            return

        session_id = context.session_id

        # Initialize buffer if needed
        if session_id not in self._buffers:
            self._buffers[session_id] = []
            self._buffer_sizes[session_id] = 0

        # Add to buffer
        self._buffers[session_id].append(context.output)
        self._buffer_sizes[session_id] += len(context.output)

        # Cancel existing flush task if any
        if session_id in self._flush_tasks:
            self._flush_tasks[session_id].cancel()

        # Check if we should flush immediately
        if self._buffer_sizes[session_id] >= self._buffer_size:
            await self._flush_buffer(session_id)
        else:
            # Schedule a delayed flush
            self._flush_tasks[session_id] = asyncio.create_task(self._delayed_flush(session_id))

    async def _handle_post_execution(self, context: HookContext) -> None:
        """Handle post execution event by flushing any remaining buffer.

        Args:
            context: The hook context
        """
        session_id = context.session_id

        # Flush any remaining buffered output
        await self._flush_buffer(session_id)

        # Send completion message with better formatting
        message = f"✅ *Execution Complete*\n\nInstance: `{context.instance_id[:8]}...`\nSession: `{session_id}`\n\nReady for next command."
        await self._send_message(session_id, message)

    async def _handle_error(self, context: HookContext) -> None:
        """Handle error event.

        Args:
            context: The hook context
        """
        # Flush any remaining buffered output first
        await self._flush_buffer(context.session_id)

        # Send error message with improved formatting
        error_text = context.error or "Unknown error"
        # Truncate very long errors
        if len(error_text) > 1000:
            error_text = error_text[:1000] + "...\n(truncated)"

        message = f"❌ *Error Occurred*\n\nInstance: `{context.instance_id[:8]}...`\n\n```\n{error_text}\n```"
        await self._send_message(context.session_id, message)

    async def _handle_agent_stop(self, context: HookContext) -> None:
        """Handle agent stop event.

        Args:
            context: The hook context
        """
        # Flush any remaining buffered output
        await self._flush_buffer(context.session_id)

        # Send stop message with better formatting
        message = f"⏹️ *Agent Stopped*\n\nInstance: `{context.instance_id[:8]}...`\nSession: `{context.session_id}`\n\nThe agent has been shut down."
        await self._send_message(context.session_id, message)

    async def _delayed_flush(self, session_id: str) -> None:
        """Flush buffer after a delay.

        Args:
            session_id: The session ID to flush
        """
        try:
            await asyncio.sleep(self._flush_interval)
            await self._flush_buffer(session_id)
        except asyncio.CancelledError:
            # Task was cancelled, don't flush
            pass

    async def _flush_buffer(self, session_id: str) -> None:
        """Flush the output buffer for a session.

        Args:
            session_id: The session ID to flush
        """
        if session_id not in self._buffers or not self._buffers[session_id]:
            return

        # Combine all buffered chunks
        text = "".join(self._buffers[session_id])

        # Clear buffer
        self._buffers[session_id] = []
        self._buffer_sizes[session_id] = 0

        # Cancel flush task if exists
        if session_id in self._flush_tasks:
            self._flush_tasks[session_id].cancel()
            del self._flush_tasks[session_id]

        # Format output in code block for better readability
        formatted_text = self._format_output(text)

        # Send the buffered text
        await self._send_message(session_id, formatted_text)

    async def _send_message(self, chat_id: str, text: str) -> None:
        """Send a message to Telegram.

        Args:
            chat_id: The Telegram chat ID
            text: The message text to send
        """
        if not text:
            return

        try:
            if self._send_callback:
                # Use provided callback
                await self._send_callback(chat_id, text)
            else:
                # Fallback: Try to import and use telegram bot
                await self._send_via_telegram_api(chat_id, text)

        except Exception as e:
            logger.error(f"Failed to send message to Telegram chat {chat_id}: {e}", exc_info=True)

    async def _send_via_telegram_api(self, chat_id: str, text: str) -> None:
        """Send message directly via Telegram API.

        Args:
            chat_id: The Telegram chat ID
            text: The message text to send
        """
        try:
            import httpx

            url = f"https://api.telegram.org/bot{self._bot_token}/sendMessage"

            # Split long messages if needed
            if len(text) > TELEGRAM_MAX_MESSAGE_LENGTH:
                chunks = self._split_message(text)
                for chunk in chunks:
                    async with httpx.AsyncClient() as client:
                        await client.post(
                            url,
                            json={
                                "chat_id": chat_id,
                                "text": chunk,
                                "parse_mode": "Markdown",
                            },
                        )
                        # Small delay between chunks to avoid rate limiting
                        await asyncio.sleep(0.5)
            else:
                async with httpx.AsyncClient() as client:
                    await client.post(
                        url,
                        json={
                            "chat_id": chat_id,
                            "text": text,
                            "parse_mode": "Markdown",
                        },
                    )

        except Exception as e:
            logger.error(f"Failed to send message via Telegram API: {e}", exc_info=True)

    def _format_output(self, text: str) -> str:
        """Format output text for Telegram display.

        Args:
            text: The raw output text

        Returns:
            Formatted text with code blocks
        """
        # Strip excessive whitespace
        text = text.strip()

        if not text:
            return text

        # Wrap in code block for monospace font
        # Use plain text block to avoid markdown parsing issues
        return f"```\n{text}\n```"

    def _split_message(self, text: str) -> list[str]:
        """Split a long message into chunks.

        Args:
            text: The text to split

        Returns:
            List of text chunks
        """
        chunks: list[str] = []
        current_chunk = ""

        for line in text.split("\n"):
            if len(current_chunk) + len(line) + 1 > self._buffer_size:
                if current_chunk:
                    chunks.append(current_chunk)
                current_chunk = line
            else:
                if current_chunk:
                    current_chunk += "\n" + line
                else:
                    current_chunk = line

        if current_chunk:
            chunks.append(current_chunk)

        return chunks


__all__ = ["TelegramHookHandler"]
