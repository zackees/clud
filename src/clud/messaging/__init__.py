"""Messaging infrastructure for multi-channel notifications (Telegram, SMS, WhatsApp)."""

from .base import MessagingClient
from .factory import create_client
from .notifier import AgentNotifier

__all__ = ["MessagingClient", "create_client", "AgentNotifier"]
