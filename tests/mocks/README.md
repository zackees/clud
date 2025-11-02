# Mock Utilities for Testing

This directory contains mock implementations and utilities for testing components that depend on external services without requiring actual network calls or API tokens.

## Telegram API Mocks

The `telegram_api.py` module provides mock implementations of the Telegram Bot API interface for unit testing.

### MockTelegramBotAPI

`MockTelegramBotAPI` is an `AsyncMock`-based implementation of `TelegramBotAPI` that allows full control over behavior and assertions in unit tests.

#### Basic Usage

```python
import unittest
from tests.mocks.telegram_api import create_mock_api

class TestMyComponent(unittest.IsolatedAsyncioTestCase):
    async def test_sends_message(self) -> None:
        # Create a mock API
        mock_api = create_mock_api()

        # Use the mock in your component
        component = MyComponent(api=mock_api)
        await component.send_greeting(chat_id=123)

        # Assert the message was sent
        mock_api.send_message.assert_called_once()
        mock_api.send_message.assert_called_with(
            chat_id=123,
            text="Hello!"
        )
```

#### Creating Mocks with Pre-configured Responses

```python
from tests.mocks.telegram_api import create_mock_with_responses
from clud.telegram.api_interface import MessageResult, TelegramUser

# Configure specific responses
mock = create_mock_with_responses({
    "send_message": MessageResult(success=False, error="Network error"),
    "get_me": TelegramUser(id=999, username="test_bot", first_name="Test Bot")
})

# Use in tests
result = await mock.send_message(chat_id=123, text="test")
assert not result.success
assert result.error == "Network error"
```

#### Testing Error Conditions

```python
from tests.mocks.telegram_api import configure_send_message_side_effect
from clud.telegram.api_interface import MessageResult

mock = create_mock_api()

# Configure to raise an exception
configure_send_message_side_effect(
    mock,
    Exception("Connection timeout")
)

# This will raise the exception
try:
    await mock.send_message(chat_id=123, text="test")
except Exception as e:
    assert str(e) == "Connection timeout"
```

#### Custom Behavior with Side Effects

```python
from tests.mocks.telegram_api import configure_send_message_side_effect
from clud.telegram.api_interface import MessageResult

mock = create_mock_api()

# Configure conditional behavior
def custom_behavior(chat_id: int, text: str, **kwargs) -> MessageResult:
    if "error" in text:
        return MessageResult(success=False, error="Simulated error")
    return MessageResult(success=True, message_id=123, chat_id=chat_id)

configure_send_message_side_effect(mock, custom_behavior)

# Test different scenarios
result1 = await mock.send_message(chat_id=123, text="Hello")
assert result1.success

result2 = await mock.send_message(chat_id=123, text="error test")
assert not result2.success
```

### Assertion Helpers

The module provides several assertion helper functions for common testing patterns.

#### assert_message_sent

Assert that a message was sent with specific content:

```python
from tests.mocks.telegram_api import create_mock_api, assert_message_sent

mock = create_mock_api()
await mock.send_message(chat_id=123, text="Hello, World!")

# Substring match (default)
assert_message_sent(mock, 123, "Hello")

# Exact match
assert_message_sent(mock, 123, "Hello, World!", exact=True)
```

#### assert_command_registered

Assert that a command handler was registered:

```python
from tests.mocks.telegram_api import create_mock_api, assert_command_registered

mock = create_mock_api()

async def start_handler(update, context):
    pass

await mock.add_command_handler("start", start_handler)

# Assert command was registered (with or without leading slash)
assert_command_registered(mock, "start")
assert_command_registered(mock, "/start")
```

#### assert_typing_sent

Assert that typing action was sent to a specific chat:

```python
from tests.mocks.telegram_api import create_mock_api, assert_typing_sent

mock = create_mock_api()
await mock.send_typing_action(chat_id=123)

assert_typing_sent(mock, 123)
```

#### get_sent_message_texts

Get all message texts sent via the mock:

```python
from tests.mocks.telegram_api import create_mock_api, get_sent_message_texts

mock = create_mock_api()
await mock.send_message(chat_id=123, text="First")
await mock.send_message(chat_id=456, text="Second")
await mock.send_message(chat_id=123, text="Third")

texts = get_sent_message_texts(mock)
assert texts == ["First", "Second", "Third"]
```

#### get_sent_messages

Get detailed information about all sent messages:

```python
from tests.mocks.telegram_api import create_mock_api, get_sent_messages

mock = create_mock_api()
await mock.send_message(
    chat_id=123,
    text="Hello",
    parse_mode="Markdown"
)
await mock.send_message(
    chat_id=456,
    text="World"
)

# Get all messages
all_messages = get_sent_messages(mock)
assert len(all_messages) == 2

# Filter by chat_id
chat_123_messages = get_sent_messages(mock, chat_id=123)
assert len(chat_123_messages) == 1
assert chat_123_messages[0]["text"] == "Hello"
assert chat_123_messages[0]["parse_mode"] == "Markdown"
```

#### get_registered_commands

Get a list of all registered command names:

```python
from tests.mocks.telegram_api import create_mock_api, get_registered_commands

mock = create_mock_api()
await mock.add_command_handler("start", handler1)
await mock.add_command_handler("help", handler2)
await mock.add_command_handler("status", handler3)

commands = get_registered_commands(mock)
assert "start" in commands
assert "help" in commands
assert "status" in commands
assert len(commands) == 3
```

### Resetting Mocks

Reset call history while preserving configured return values:

```python
from tests.mocks.telegram_api import create_mock_api, reset_mock

mock = create_mock_api()
await mock.send_message(chat_id=123, text="Hello")
assert mock.send_message.call_count == 1

reset_mock(mock)
assert mock.send_message.call_count == 0

# Return values are still configured
result = await mock.send_message(chat_id=123, text="Test")
assert result.success
```

## Complete Test Example

Here's a complete example showing common testing patterns:

```python
import unittest
from tests.mocks.telegram_api import (
    create_mock_api,
    assert_message_sent,
    assert_command_registered,
    get_sent_message_texts,
    reset_mock,
)
from my_component import TelegramBot

class TestTelegramBot(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.mock_api = create_mock_api()
        self.bot = TelegramBot(api=self.mock_api)
        await self.bot.initialize()

    async def asyncTearDown(self) -> None:
        reset_mock(self.mock_api)

    async def test_initialization(self) -> None:
        # Verify initialize was called
        self.mock_api.initialize.assert_called_once()

    async def test_registers_commands(self) -> None:
        # Set up command handlers
        await self.bot.setup_handlers()

        # Assert all expected commands are registered
        assert_command_registered(self.mock_api, "start")
        assert_command_registered(self.mock_api, "help")
        assert_command_registered(self.mock_api, "status")

    async def test_sends_welcome_message(self) -> None:
        # Send welcome message
        await self.bot.send_welcome(chat_id=123, username="testuser")

        # Assert message was sent with expected content
        assert_message_sent(self.mock_api, 123, "Welcome")
        assert_message_sent(self.mock_api, 123, "testuser")

    async def test_handles_multiple_messages(self) -> None:
        # Send multiple messages
        await self.bot.send_welcome(chat_id=123, username="user1")
        await self.bot.send_welcome(chat_id=456, username="user2")

        # Verify all messages were sent
        texts = get_sent_message_texts(self.mock_api)
        assert len(texts) == 2
        assert "user1" in texts[0]
        assert "user2" in texts[1]

    async def test_error_handling(self) -> None:
        # Configure mock to fail
        from tests.mocks.telegram_api import configure_send_message_side_effect
        from clud.telegram.api_interface import MessageResult

        configure_send_message_side_effect(
            self.mock_api,
            MessageResult(success=False, error="Network error")
        )

        # Attempt to send message
        result = await self.bot.send_message(chat_id=123, text="test")

        # Verify error handling
        self.assertFalse(result.success)
        self.assertEqual(result.error, "Network error")

if __name__ == "__main__":
    unittest.main()
```

## Best Practices

### 1. Use Factory Functions

Always use `create_mock_api()` or `create_mock_with_responses()` instead of directly instantiating `MockTelegramBotAPI`:

```python
# Good
mock = create_mock_api()

# Avoid
mock = MockTelegramBotAPI()
```

### 2. Reset Mocks Between Tests

Use `reset_mock()` in `asyncTearDown()` to ensure test isolation:

```python
async def asyncTearDown(self) -> None:
    reset_mock(self.mock_api)
```

### 3. Use Assertion Helpers

Prefer assertion helpers over direct mock assertions for clarity:

```python
# Good
assert_message_sent(mock, 123, "Hello")

# Less clear
mock.send_message.assert_called()
call_args = mock.send_message.call_args
assert call_args[1]["chat_id"] == 123
assert "Hello" in call_args[1]["text"]
```

### 4. Test Both Success and Failure Paths

Always test both successful operations and error conditions:

```python
async def test_success_path(self) -> None:
    result = await self.bot.send_message(chat_id=123, text="test")
    assert result.success

async def test_failure_path(self) -> None:
    configure_send_message_side_effect(
        self.mock_api,
        MessageResult(success=False, error="Failed")
    )
    result = await self.bot.send_message(chat_id=123, text="test")
    assert not result.success
```

### 5. Use Specific Assertions

Be as specific as possible in your assertions:

```python
# Good - specific assertion
assert_message_sent(mock, 123, "Welcome, John", exact=True)

# Less specific
assert_message_sent(mock, 123, "Welcome")
```

## Comparison with Fake Implementation

| Feature | Mock (unittest.mock) | Fake (FakeTelegramBotAPI) |
|---------|---------------------|---------------------------|
| Use Case | Unit tests | Integration tests |
| Network Calls | None | None |
| State Management | None (stateless) | Full (messages, handlers) |
| Behavior | Configured per test | Realistic simulation |
| Complexity | Simple | Complex |
| Setup Time | Fast | Moderate |
| Assertions | Mock-based | State-based |

**When to use Mock**:
- Unit testing individual components
- Testing error conditions
- Fast, isolated tests
- Simple behavior configuration

**When to use Fake**:
- Integration testing
- Testing interaction between components
- Realistic message flow simulation
- Handler routing testing

## Additional Resources

- [Python unittest.mock Documentation](https://docs.python.org/3/library/unittest.mock.html)
- [AsyncMock Documentation](https://docs.python.org/3/library/unittest.mock.html#unittest.mock.AsyncMock)
- [Telegram Bot API Interface](../src/clud/telegram/api_interface.py)
- [Fake Telegram API Implementation](../src/clud/telegram/api_fake.py)
