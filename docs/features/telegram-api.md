# Telegram Bot API Abstraction

The Telegram integration uses an abstract API interface that allows for testing without real Telegram bot tokens or network calls.

## API Implementations (`src/clud/telegram/`)

### TelegramBotAPI (`api_interface.py`)

Abstract base class defining the interface.

- Provides type-safe abstractions for Telegram operations
- No direct dependency on `python-telegram-bot` library types
- All methods are async and fully typed

### RealTelegramBotAPI (`api_real.py`)

Production implementation.

- Wraps `python-telegram-bot` library
- Converts between abstract types and telegram library types
- Handles real Telegram API calls

### FakeTelegramBotAPI (`api_fake.py`)

In-memory testing implementation.

- Simulates Telegram bot behavior without network calls
- Stores messages in memory for inspection
- Configurable latency and error injection
- Deterministic behavior for reliable tests

### MockTelegramBotAPI (`tests/mocks/telegram_api.py`)

Mock utilities for testing.

- Based on `unittest.mock.AsyncMock`
- Helper functions for common assertions
- Pre-configured mock builders

## Configuration

### TelegramAPIConfig (`api_config.py`)

Configuration for API implementation mode:

- `implementation`: "real", "fake", or "mock"
- `bot_token`: Telegram bot token (required for "real" mode)
- `fake_delay_ms`: Delay in ms for fake mode (default: 100)
- `fake_error_rate`: Error rate 0.0-1.0 for fake mode (default: 0.0)

### TelegramIntegrationConfig (`config.py`)

Main configuration with `api` field:

- Integrates API config with telegram, web, sessions, and logging config
- Supports loading from environment variables, files, or defaults

## Factory (`api_factory.py`)

### create_telegram_api()

Creates appropriate implementation based on config:

- Auto-detects mode from environment variables
- Defaults to "fake" when no token provided
- Defaults to "real" when token provided
- Supports explicit override via `TELEGRAM_API_MODE`

## Environment Variables

```bash
# Telegram API Mode Selection
export TELEGRAM_API_MODE=fake           # "real" | "fake" | "mock"
export TELEGRAM_BOT_TOKEN=<token>       # Required for "real" mode
export TELEGRAM_FAKE_DELAY=100          # Delay in ms for fake mode (default: 100)
export TELEGRAM_FAKE_ERROR_RATE=0.0     # Error rate 0.0-1.0 for fake mode (default: 0.0)
```

## Testing with Fake API

```python
from clud.telegram.api_config import TelegramAPIConfig
from clud.telegram.api_factory import create_telegram_api

# Create fake API for testing (zero delay, no errors)
config = TelegramAPIConfig.for_testing(implementation="fake")
api = create_telegram_api(config=config)

# Send messages and inspect results
await api.send_message(chat_id="12345", text="Hello!")
messages = api.get_sent_messages("12345")
assert len(messages) == 1
assert messages[0].text == "Hello!"
```

## Test Coverage

- `tests/test_telegram_api_interface.py` - Interface and config tests
- `tests/test_telegram_api_fake.py` - Fake implementation tests (17 tests)
- `tests/test_telegram_bot_handler_integration.py` - Bot handler with fake API (12 tests)
- `tests/test_telegram_hook_handler_integration.py` - Hook handler with fake API (9 tests)
- `tests/test_telegram_messenger_integration.py` - Messenger with fake API (6 tests)
- `tests/test_telegram_e2e.py` - End-to-end integration tests (15 tests)

## Benefits

- ✅ Test telegram functionality without network calls or bot tokens
- ✅ Deterministic test behavior (no flaky tests)
- ✅ Fast test execution (no real API latency)
- ✅ Type-safe abstractions (no third-party library type issues)
- ✅ Easy to swap implementations (real ↔ fake ↔ mock)
- ✅ Comprehensive test coverage across all telegram components

## Telegram Bot Integration

### Quick Start

```bash
# Open Telegram bot landing page
clud --telegram
# or
clud -tg
```

### Features

- Launches a local HTTP server on an auto-assigned port
- Opens the landing page in your default browser
- Landing page provides:
  - Button to open the Claude Code Telegram bot (https://t.me/clud_ckl_bot)
  - Explanation of why direct iframe embedding isn't possible (Telegram security)
  - Preview of upcoming features (custom chat UI, dashboard, etc.)
- Press Ctrl+C to stop the server

### Note

Telegram blocks iframe embedding with X-Frame-Options for security, so the landing page provides a button to open the bot in Telegram instead.

## Related Documentation

- [Hooks and Message Handler API](hooks.md)
- [Web UI](webui.md)
- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
