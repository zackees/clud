"""
Mock utilities for Telegram API testing.

This module provides mock implementations and utilities for testing
components that depend on the Telegram Bot API without requiring
actual network calls or bot tokens.
"""

from collections.abc import Callable
from typing import Any
from unittest.mock import AsyncMock, Mock

from clud.telegram.api_interface import (
    MessageResult,
    TelegramBotAPI,
    TelegramUser,
)


class MockTelegramBotAPI(AsyncMock):
    """
    Mock implementation of TelegramBotAPI using AsyncMock.

    This class extends AsyncMock to provide a fully mockable
    TelegramBotAPI implementation for unit testing. All methods
    are AsyncMock instances by default, allowing full control
    over their behavior and assertions.

    Example:
        >>> mock_api = MockTelegramBotAPI()
        >>> mock_api.send_message.return_value = MessageResult(
        ...     success=True, message_id=123
        ... )
        >>> result = await mock_api.send_message(chat_id=456, text="Hello")
        >>> assert result.success
        >>> mock_api.send_message.assert_called_once()
    """

    def __init__(self, **kwargs: Any) -> None:
        """
        Initialize the mock Telegram API.

        Args:
            **kwargs: Additional keyword arguments passed to AsyncMock
        """
        super().__init__(spec=TelegramBotAPI, **kwargs)

        # Configure default return values for common methods
        self.initialize = AsyncMock(return_value=True)
        self.send_message = AsyncMock(return_value=MessageResult(success=True, message_id=1))
        self.send_typing_action = AsyncMock(return_value=True)
        self.get_me = AsyncMock(return_value=TelegramUser(id=12345, username="test_bot", first_name="Test Bot"))
        self.start_polling = AsyncMock(return_value=None)
        self.stop_polling = AsyncMock(return_value=None)
        self.shutdown = AsyncMock(return_value=None)
        self.add_command_handler = AsyncMock(return_value=None)
        self.add_message_handler = AsyncMock(return_value=None)
        self.add_error_handler = AsyncMock(return_value=None)


def create_mock_api(**kwargs: Any) -> MockTelegramBotAPI:
    """
    Create a pre-configured MockTelegramBotAPI instance.

    This factory function creates a MockTelegramBotAPI with sensible
    default configurations for common testing scenarios.

    Args:
        **kwargs: Additional keyword arguments passed to MockTelegramBotAPI

    Returns:
        Configured MockTelegramBotAPI instance

    Example:
        >>> mock = create_mock_api()
        >>> await mock.initialize()
        >>> mock.initialize.assert_called_once()
    """
    return MockTelegramBotAPI(**kwargs)


def create_mock_with_responses(responses: dict[str, Any]) -> MockTelegramBotAPI:
    """
    Create a MockTelegramBotAPI with pre-configured responses.

    This factory function creates a mock with specific return values
    for different methods, making it easy to test various scenarios.

    Args:
        responses: Dictionary mapping method names to return values
                  Example: {"send_message": MessageResult(...)}

    Returns:
        Configured MockTelegramBotAPI instance

    Example:
        >>> mock = create_mock_with_responses({
        ...     "send_message": MessageResult(success=False, error="Network error"),
        ...     "get_me": TelegramUser(id=999, username="bot", first_name="Bot")
        ... })
        >>> result = await mock.send_message(chat_id=123, text="test")
        >>> assert not result.success
    """
    mock = MockTelegramBotAPI()

    for method_name, return_value in responses.items():
        if hasattr(mock, method_name):
            method = getattr(mock, method_name)
            if isinstance(method, (AsyncMock, Mock)):
                method.return_value = return_value

    return mock


def assert_message_sent(
    mock: MockTelegramBotAPI,
    chat_id: int | str,
    text: str,
    *,
    exact: bool = False,
) -> None:
    """
    Assert that a message was sent with specific content.

    Args:
        mock: The MockTelegramBotAPI instance to check
        chat_id: Expected chat ID
        text: Expected message text (substring match unless exact=True)
        exact: If True, requires exact text match; if False, substring match

    Raises:
        AssertionError: If the message was not sent as expected

    Example:
        >>> mock = create_mock_api()
        >>> await mock.send_message(chat_id=123, text="Hello, World!")
        >>> assert_message_sent(mock, 123, "Hello")  # Substring match
        >>> assert_message_sent(mock, 123, "Hello, World!", exact=True)  # Exact match
    """
    # Get all calls to send_message
    calls = mock.send_message.call_args_list

    if not calls:
        raise AssertionError("send_message was never called")

    # Check if any call matches the criteria
    for call_obj in calls:
        call_args, call_kwargs = call_obj
        called_chat_id = call_kwargs.get("chat_id") or (call_args[0] if len(call_args) > 0 else None)
        called_text = call_kwargs.get("text") or (call_args[1] if len(call_args) > 1 else None)

        # Check chat_id match
        if str(called_chat_id) != str(chat_id):
            continue

        # Check text match
        if called_text is None:
            continue

        if exact:
            if called_text == text:
                return
        else:
            if text in called_text:
                return

    # If we get here, no matching call was found
    match_type = "exact" if exact else "substring"
    raise AssertionError(f"No call to send_message found with chat_id={chat_id} and {match_type} text='{text}'. Calls: {calls}")


def assert_command_registered(mock: MockTelegramBotAPI, command: str) -> None:
    """
    Assert that a command handler was registered.

    Args:
        mock: The MockTelegramBotAPI instance to check
        command: Command name to check (without leading slash)

    Raises:
        AssertionError: If the command was not registered

    Example:
        >>> mock = create_mock_api()
        >>> await mock.add_command_handler("start", some_handler)
        >>> assert_command_registered(mock, "start")
    """
    # Get all calls to add_command_handler
    calls = mock.add_command_handler.call_args_list

    if not calls:
        raise AssertionError("add_command_handler was never called")

    # Normalize command (remove leading slash if present)
    normalized_command = command.lstrip("/")

    # Check if any call matches the command
    for call_obj in calls:
        call_args, call_kwargs = call_obj
        called_command = call_kwargs.get("command") or (call_args[0] if len(call_args) > 0 else None)

        if called_command is None:
            continue

        # Normalize called command
        normalized_called = called_command.lstrip("/")

        if normalized_called == normalized_command:
            return

    # If we get here, the command was not registered
    raise AssertionError(f"Command '{command}' was not registered. Calls: {calls}")


def get_sent_message_texts(mock: MockTelegramBotAPI) -> list[str]:
    """
    Get a list of all message texts sent via send_message.

    Args:
        mock: The MockTelegramBotAPI instance to check

    Returns:
        List of message texts in the order they were sent

    Example:
        >>> mock = create_mock_api()
        >>> await mock.send_message(chat_id=123, text="First")
        >>> await mock.send_message(chat_id=123, text="Second")
        >>> texts = get_sent_message_texts(mock)
        >>> assert texts == ["First", "Second"]
    """
    calls = mock.send_message.call_args_list
    texts: list[str] = []

    for call_obj in calls:
        call_args, call_kwargs = call_obj
        text = call_kwargs.get("text") or (call_args[1] if len(call_args) > 1 else None)

        if text is not None:
            texts.append(text)

    return texts


def get_sent_messages(mock: MockTelegramBotAPI, chat_id: int | str | None = None) -> list[dict[str, Any]]:
    """
    Get detailed information about all sent messages.

    Args:
        mock: The MockTelegramBotAPI instance to check
        chat_id: Optional chat ID to filter messages

    Returns:
        List of dictionaries containing message details (chat_id, text, parse_mode, etc.)

    Example:
        >>> mock = create_mock_api()
        >>> await mock.send_message(chat_id=123, text="Hello", parse_mode="Markdown")
        >>> messages = get_sent_messages(mock, chat_id=123)
        >>> assert messages[0]["text"] == "Hello"
        >>> assert messages[0]["parse_mode"] == "Markdown"
    """
    calls = mock.send_message.call_args_list
    messages: list[dict[str, Any]] = []

    for call_obj in calls:
        call_args, call_kwargs = call_obj

        # Extract all parameters
        message_chat_id = call_kwargs.get("chat_id") or (call_args[0] if len(call_args) > 0 else None)
        text = call_kwargs.get("text") or (call_args[1] if len(call_args) > 1 else None)
        parse_mode = call_kwargs.get("parse_mode")
        reply_to_message_id = call_kwargs.get("reply_to_message_id")

        # Filter by chat_id if specified
        if chat_id is not None and str(message_chat_id) != str(chat_id):
            continue

        messages.append(
            {
                "chat_id": message_chat_id,
                "text": text,
                "parse_mode": parse_mode,
                "reply_to_message_id": reply_to_message_id,
            }
        )

    return messages


def assert_typing_sent(mock: MockTelegramBotAPI, chat_id: int | str) -> None:
    """
    Assert that typing action was sent to a specific chat.

    Args:
        mock: The MockTelegramBotAPI instance to check
        chat_id: Expected chat ID

    Raises:
        AssertionError: If typing action was not sent to the chat

    Example:
        >>> mock = create_mock_api()
        >>> await mock.send_typing_action(chat_id=123)
        >>> assert_typing_sent(mock, 123)
    """
    calls = mock.send_typing_action.call_args_list

    if not calls:
        raise AssertionError("send_typing_action was never called")

    # Check if any call matches the chat_id
    for call_obj in calls:
        call_args, call_kwargs = call_obj
        called_chat_id = call_kwargs.get("chat_id") or (call_args[0] if len(call_args) > 0 else None)

        if str(called_chat_id) == str(chat_id):
            return

    # If we get here, typing was not sent to this chat
    raise AssertionError(f"send_typing_action was not called with chat_id={chat_id}. Calls: {calls}")


def get_registered_commands(mock: MockTelegramBotAPI) -> list[str]:
    """
    Get a list of all registered command names.

    Args:
        mock: The MockTelegramBotAPI instance to check

    Returns:
        List of command names (without leading slashes)

    Example:
        >>> mock = create_mock_api()
        >>> await mock.add_command_handler("start", handler1)
        >>> await mock.add_command_handler("help", handler2)
        >>> commands = get_registered_commands(mock)
        >>> assert "start" in commands
        >>> assert "help" in commands
    """
    calls = mock.add_command_handler.call_args_list
    commands: list[str] = []

    for call_obj in calls:
        call_args, call_kwargs = call_obj
        command = call_kwargs.get("command") or (call_args[0] if len(call_args) > 0 else None)

        if command is not None:
            # Normalize command (remove leading slash)
            normalized = command.lstrip("/")
            commands.append(normalized)

    return commands


def reset_mock(mock: MockTelegramBotAPI) -> None:
    """
    Reset all call history on the mock while preserving configured return values.

    Args:
        mock: The MockTelegramBotAPI instance to reset

    Example:
        >>> mock = create_mock_api()
        >>> await mock.send_message(chat_id=123, text="Hello")
        >>> reset_mock(mock)
        >>> assert mock.send_message.call_count == 0
    """
    mock.reset_mock()


def configure_send_message_side_effect(mock: MockTelegramBotAPI, side_effect: Callable[..., MessageResult] | Exception) -> None:
    """
    Configure send_message to use a side effect (function or exception).

    Args:
        mock: The MockTelegramBotAPI instance to configure
        side_effect: Function to call or exception to raise

    Example:
        >>> mock = create_mock_api()
        >>> def custom_behavior(chat_id, text, **kwargs):
        ...     if "error" in text:
        ...         return MessageResult(success=False, error="Simulated error")
        ...     return MessageResult(success=True, message_id=123)
        >>> configure_send_message_side_effect(mock, custom_behavior)
    """
    mock.send_message.side_effect = side_effect
