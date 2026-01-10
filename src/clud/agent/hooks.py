"""Hook system integration for agent operations.

This module provides utilities for registering and triggering hooks from the agent.
Hooks allow external systems (Telegram, webhooks, etc.) to receive agent events.
"""

import sys
import traceback

from clud.hooks import HookContext, HookEvent, get_hook_manager
from clud.hooks.config import load_hook_config


def register_hooks_from_config(hook_debug: bool = False) -> None:
    """Register hooks based on configuration file.

    Loads hook configuration from ~/.clud/hooks.json and registers
    enabled handlers with the HookManager.

    Args:
        hook_debug: Whether to print debug information
    """
    try:
        # Load hook configuration
        config = load_hook_config()

        if not config.enabled:
            if hook_debug:
                print("DEBUG: Hooks disabled in configuration", file=sys.stderr)
            return

        hook_manager = get_hook_manager()

        # Register Telegram hook if enabled
        if config.telegram_enabled and config.telegram_bot_token and config.telegram_chat_id:
            # Lazy import to avoid loading telegram/fastapi unless actually needed
            from clud.hooks.telegram import TelegramHookHandler

            telegram_handler = TelegramHookHandler(
                bot_token=config.telegram_bot_token,
                buffer_size=config.buffer_size,
                flush_interval=config.flush_interval,
            )
            hook_manager.register(telegram_handler)
            if hook_debug:
                print("DEBUG: Registered Telegram hook (will use session_id as chat_id)", file=sys.stderr)

        # Register webhook hook if enabled
        if config.webhook_enabled and config.webhook_url:
            # Lazy import to avoid loading webhook dependencies unless actually needed
            from clud.hooks.webhook import WebhookHookHandler

            webhook_handler = WebhookHookHandler(
                webhook_url=config.webhook_url,
                secret=config.webhook_secret,
            )
            hook_manager.register(webhook_handler)
            if hook_debug:
                print(f"DEBUG: Registered webhook hook (url={config.webhook_url})", file=sys.stderr)

    except KeyboardInterrupt:
        raise
    except Exception as e:
        if hook_debug:
            print(f"DEBUG: Failed to register hooks: {e}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
        # Don't fail if hooks can't be registered - hooks are optional


def trigger_hook_sync(event: HookEvent, context: HookContext, hook_debug: bool = False) -> None:
    """Trigger a hook event synchronously.

    Args:
        event: The hook event type
        context: The hook context
        hook_debug: Whether to print debug info
    """
    try:
        hook_manager = get_hook_manager()

        # Skip if no handlers registered
        if not hook_manager.has_handlers(event):
            if hook_debug:
                print(f"DEBUG: No handlers for event {event.value}", file=sys.stderr)
            return

        if hook_debug:
            print(f"DEBUG: Triggering hook event: {event.value}", file=sys.stderr)

        # Trigger synchronously - just pass context (event is inside context)
        hook_manager.trigger_sync(context)

    except KeyboardInterrupt:
        raise
    except Exception as e:
        if hook_debug:
            print(f"DEBUG: Hook trigger failed: {e}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
        # Don't fail if hooks fail - they are optional
