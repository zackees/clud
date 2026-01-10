"""Telegram integration package for clud.

This package provides advanced Telegram bot integration with web-based client
interface for monitoring and interacting with Telegram bot conversations.

NOTE: This module uses lazy loading to avoid importing heavy dependencies
(fastapi, pydantic, etc.) until they are actually needed. Import from specific
submodules (e.g., clud.telegram.api_interface) for better performance.
"""

from typing import Any

# Lazy-loading proxy pattern to avoid importing fastapi/pydantic at module load time
# All imports moved inside functions to defer loading until actually needed


def create_telegram_api_router(session_manager: Any, auth_token: str | None = None) -> Any:
    """Create Telegram API router (lazy-loaded).

    Args:
        session_manager: The session manager instance
        auth_token: Optional auth token for protected endpoints

    Returns:
        FastAPI APIRouter instance
    """
    from clud.telegram.api import create_telegram_api_router as _create

    return _create(session_manager, auth_token)


def run_telegram_server(*args: Any, **kwargs: Any) -> Any:
    """Run Telegram server (lazy-loaded).

    Args:
        *args: Positional arguments for run_telegram_server
        **kwargs: Keyword arguments for run_telegram_server

    Returns:
        Result from run_telegram_server
    """
    from clud.telegram.server import run_telegram_server as _run

    return _run(*args, **kwargs)


def telegram_websocket_endpoint(*args: Any, **kwargs: Any) -> Any:
    """Telegram WebSocket endpoint (lazy-loaded).

    Args:
        *args: Positional arguments for telegram_websocket_endpoint
        **kwargs: Keyword arguments for telegram_websocket_endpoint

    Returns:
        Result from telegram_websocket_endpoint
    """
    from clud.telegram.ws_server import telegram_websocket_endpoint as _endpoint

    return _endpoint(*args, **kwargs)


class TelegramMessage:
    """Lazy-loading proxy for TelegramMessage."""

    def __new__(cls, *args: Any, **kwargs: Any) -> Any:
        """Create actual TelegramMessage instance.

        Args:
            *args: Positional arguments for TelegramMessage
            **kwargs: Keyword arguments for TelegramMessage

        Returns:
            TelegramMessage instance
        """
        from clud.telegram.models import TelegramMessage as _TelegramMessage

        return _TelegramMessage(*args, **kwargs)


class TelegramSession:
    """Lazy-loading proxy for TelegramSession."""

    def __new__(cls, *args: Any, **kwargs: Any) -> Any:
        """Create actual TelegramSession instance.

        Args:
            *args: Positional arguments for TelegramSession
            **kwargs: Keyword arguments for TelegramSession

        Returns:
            TelegramSession instance
        """
        from clud.telegram.models import TelegramSession as _TelegramSession

        return _TelegramSession(*args, **kwargs)


class TelegramBotHandler:
    """Lazy-loading proxy for TelegramBotHandler."""

    def __new__(cls, *args: Any, **kwargs: Any) -> Any:
        """Create actual TelegramBotHandler instance.

        Args:
            *args: Positional arguments for TelegramBotHandler
            **kwargs: Keyword arguments for TelegramBotHandler

        Returns:
            TelegramBotHandler instance
        """
        from clud.telegram.bot_handler import TelegramBotHandler as _TelegramBotHandler

        return _TelegramBotHandler(*args, **kwargs)


class TelegramIntegrationConfig:
    """Lazy-loading proxy for TelegramIntegrationConfig."""

    def __new__(cls, *args: Any, **kwargs: Any) -> Any:
        """Create actual TelegramIntegrationConfig instance.

        Args:
            *args: Positional arguments for TelegramIntegrationConfig
            **kwargs: Keyword arguments for TelegramIntegrationConfig

        Returns:
            TelegramIntegrationConfig instance
        """
        from clud.telegram.config import TelegramIntegrationConfig as _TelegramIntegrationConfig

        return _TelegramIntegrationConfig(*args, **kwargs)


class SessionManager:
    """Lazy-loading proxy for SessionManager."""

    def __new__(cls, *args: Any, **kwargs: Any) -> Any:
        """Create actual SessionManager instance.

        Args:
            *args: Positional arguments for SessionManager
            **kwargs: Keyword arguments for SessionManager

        Returns:
            SessionManager instance
        """
        from clud.telegram.session_manager import SessionManager as _SessionManager

        return _SessionManager(*args, **kwargs)


class TelegramWebSocketHandler:
    """Lazy-loading proxy for TelegramWebSocketHandler."""

    def __new__(cls, *args: Any, **kwargs: Any) -> Any:
        """Create actual TelegramWebSocketHandler instance.

        Args:
            *args: Positional arguments for TelegramWebSocketHandler
            **kwargs: Keyword arguments for TelegramWebSocketHandler

        Returns:
            TelegramWebSocketHandler instance
        """
        from clud.telegram.ws_server import TelegramWebSocketHandler as _TelegramWebSocketHandler

        return _TelegramWebSocketHandler(*args, **kwargs)


class TelegramServer:
    """Lazy-loading proxy for TelegramServer."""

    def __new__(cls, *args: Any, **kwargs: Any) -> Any:
        """Create actual TelegramServer instance.

        Args:
            *args: Positional arguments for TelegramServer
            **kwargs: Keyword arguments for TelegramServer

        Returns:
            TelegramServer instance
        """
        from clud.telegram.server import TelegramServer as _TelegramServer

        return _TelegramServer(*args, **kwargs)


__all__ = [
    "TelegramMessage",
    "TelegramSession",
    "TelegramBotHandler",
    "TelegramIntegrationConfig",
    "SessionManager",
    "TelegramWebSocketHandler",
    "telegram_websocket_endpoint",
    "create_telegram_api_router",
    "TelegramServer",
    "run_telegram_server",
]
