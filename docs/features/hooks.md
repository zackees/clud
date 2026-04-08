# Hook System

The hook system provides an event-based architecture for intercepting and forwarding execution events to external systems (webhooks, etc.).

## Hook System (`src/clud/hooks/`)

### Core Components

- **HookManager**: Singleton that manages hook registration and event triggering
- **HookHandler Protocol**: Interface for implementing custom hook handlers
- **WebhookHandler**: Built-in handler for HTTP webhook notifications

### Hook Events

- **PRE_EXECUTION**: Before Claude Code starts execution
- **POST_EXECUTION**: After Claude Code completes execution
- **OUTPUT_CHUNK**: Real-time output streaming chunks
- **ERROR**: When an error occurs during execution
- **AGENT_START**: When agent subprocess starts
- **AGENT_STOP**: When agent subprocess stops

### Claude-Compatible Mapping

`clud` also understands Claude-style hook config names from `.claude/settings.json` and `.claude/settings.local.json`.

| Config hook name | Internal event | Runtime meaning |
| --- | --- | --- |
| `Start` | `AGENT_START` | Agent session is starting |
| `Stop` | `POST_EXECUTION` | Agent finished a normal execution turn |
| `SessionEnd` | `AGENT_STOP` | Agent session is shutting down |

This is the main source of confusion:

- `Stop` does **not** map to `AGENT_STOP`
- `SessionEnd` is the true final lifecycle hook

### Hook Control Flags

- `--no-hooks`: disable all hook registration and all hook execution
- `--no-session-end-hook`: disable only the final `SessionEnd` / `AGENT_STOP` hook
- `--no-stop-hook`: deprecated alias for `--no-session-end-hook`

### Implementation

```python
from clud.hooks import HookManager, HookHandler, HookEvent, HookContext

# Create custom hook handler
class MyHookHandler(HookHandler):
    def on_event(self, event: HookEvent, context: HookContext) -> None:
        if event == HookEvent.OUTPUT_CHUNK:
            print(f"Output: {context.data}")

# Register handler
hook_manager = HookManager.get_instance()
hook_manager.register_handler(MyHookHandler())

# Trigger event
hook_manager.trigger(HookEvent.OUTPUT_CHUNK, {"data": "Hello, world!"})
```

## Testing

- `tests/test_hooks.py` - Hook system unit tests

## Architecture

### Hook System Data Flow

```
Execution Event → HookManager → Registered Handlers
                                    ↓
                            Webhook / Custom
```

## Configuration

### Hook Configuration (`hooks/config.py`)

```python
from clud.hooks.config import HookConfig

config = HookConfig(
    webhook_enabled=True,
    webhook_url="https://example.com/webhook"
)
```

## Related Documentation

- [Development Setup](../development/setup.md)
- [Architecture](../development/architecture.md)
