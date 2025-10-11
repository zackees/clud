"""Abstract base class for all messaging clients."""

from abc import ABC, abstractmethod


class MessagingClient(ABC):
    """Abstract base for all messaging clients."""

    @abstractmethod
    async def send_message(self, contact: str, message: str) -> bool:
        """Send a text message.

        Args:
            contact: User contact identifier (phone, username, chat_id)
            message: Text message to send

        Returns:
            True if message sent successfully, False otherwise
        """
        pass

    @abstractmethod
    async def send_code_block(self, contact: str, code: str, language: str = "python") -> bool:
        """Send formatted code block.

        Args:
            contact: User contact identifier
            code: Code content to send
            language: Programming language for syntax highlighting

        Returns:
            True if message sent successfully, False otherwise
        """
        pass

    async def send_error(self, contact: str, error: str) -> bool:
        """Send error message with appropriate formatting.

        Args:
            contact: User contact identifier
            error: Error message to send

        Returns:
            True if message sent successfully, False otherwise
        """
        message = f"⚠️ **Error**\n\n{error}"
        return await self.send_message(contact, message)

    async def get_user_response(self, contact: str, timeout: int = 60) -> str | None:
        """Wait for user response (optional feature).

        Args:
            contact: User contact identifier
            timeout: Maximum seconds to wait for response

        Returns:
            User's response text, or None if timeout/not supported
        """
        # Default implementation: not supported
        return None
