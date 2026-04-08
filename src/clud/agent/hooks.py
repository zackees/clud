"""Hook system integration for agent operations."""

import sys
import traceback
from dataclasses import dataclass
from pathlib import Path

from clud.hooks import HookContext, HookEvent, get_hook_manager
from clud.hooks.claude_compat import load_claude_compat_hooks
from clud.hooks.command import CommandHookHandler
from clud.hooks.config import load_hook_config
from clud.util import handle_keyboard_interrupt


@dataclass(slots=True)
class HookRegistrationSummary:
    """Summary of which hook families were registered."""

    has_start_hooks: bool = False
    has_post_execution_hooks: bool = False
    has_session_end_hooks: bool = False

    @property
    def has_stop_hooks(self) -> bool:
        """Backward-compatible alias for Claude-compatible Stop hooks."""
        return self.has_post_execution_hooks

    @has_stop_hooks.setter
    def has_stop_hooks(self, value: bool) -> None:
        """Backward-compatible alias for Claude-compatible Stop hooks."""
        self.has_post_execution_hooks = value


def register_hooks_from_config(hook_debug: bool = False, cwd: Path | None = None) -> HookRegistrationSummary:
    """Register hooks based on configuration file.

    Loads hook configuration from ~/.clud/hooks.json and registers
    enabled handlers with the HookManager.

    Args:
        hook_debug: Whether to print debug information
    """
    summary = HookRegistrationSummary()

    try:
        # Load hook configuration
        config = load_hook_config()

        if config.enabled:
            hook_manager = get_hook_manager()

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
        elif hook_debug:
            print("DEBUG: Hooks disabled in configuration", file=sys.stderr)

        compat = load_claude_compat_hooks(cwd=cwd)
        if compat.start or compat.stop or compat.session_end:
            hook_manager = get_hook_manager()
            if compat.start:
                hook_manager.register(CommandHookHandler(compat.start), [HookEvent.AGENT_START])
                summary.has_start_hooks = True
                if hook_debug:
                    print(f"DEBUG: Registered {len(compat.start)} Claude-compatible Start hook(s)", file=sys.stderr)
            if compat.stop:
                hook_manager.register(CommandHookHandler(compat.stop), [HookEvent.POST_EXECUTION])
                summary.has_post_execution_hooks = True
                if hook_debug:
                    print(f"DEBUG: Registered {len(compat.stop)} Claude-compatible Stop hook(s)", file=sys.stderr)
            if compat.session_end:
                hook_manager.register(CommandHookHandler(compat.session_end), [HookEvent.AGENT_STOP])
                summary.has_session_end_hooks = True
                if hook_debug:
                    print(
                        f"DEBUG: Registered {len(compat.session_end)} Claude-compatible SessionEnd hook(s)",
                        file=sys.stderr,
                    )

        return summary

    except KeyboardInterrupt as e:
        handle_keyboard_interrupt(e)
    except Exception as e:
        if hook_debug:
            print(f"DEBUG: Failed to register hooks: {e}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
        # Don't fail if hooks can't be registered - hooks are optional
    return summary


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

    except KeyboardInterrupt as e:
        handle_keyboard_interrupt(e)
    except Exception as e:
        if hook_debug:
            print(f"DEBUG: Hook trigger failed: {e}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
        # Don't fail if hooks fail - they are optional
