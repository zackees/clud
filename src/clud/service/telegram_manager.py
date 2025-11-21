"""Telegram service lifecycle management."""

import asyncio
import logging
import threading
from typing import Any

logger = logging.getLogger(__name__)


class TelegramServiceManager:
    """Manages telegram service lifecycle within the daemon."""

    def __init__(self) -> None:
        """Initialize telegram service manager."""
        self.is_running = False
        self.server_thread: threading.Thread | None = None
        self.telegram_server: Any = None  # TelegramServer instance
        self.asyncio_loop: asyncio.AbstractEventLoop | None = None
        self.config: Any = None  # TelegramIntegrationConfig
        logger.debug("TelegramServiceManager initialized")

    def get_status(self) -> dict[str, Any]:
        """Get telegram service status.

        Returns:
            Status dictionary with running state and config info
        """
        status: dict[str, Any] = {"running": self.is_running}
        if self.is_running and self.config:
            status["port"] = self.config.web.port
            status["host"] = self.config.web.host
            status["bot_configured"] = bool(self.config.telegram.bot_token)
        return status

    def start_service(self, config_path: str | None = None, port: int | None = None) -> bool:
        """Start the telegram service.

        Args:
            config_path: Optional path to telegram config file
            port: Optional port override

        Returns:
            True if started successfully, False otherwise
        """
        if self.is_running:
            logger.warning("Telegram service already running")
            return False

        logger.info("Starting telegram service...")

        try:
            # Import telegram modules (lazy import to avoid dependency issues)
            from clud.telegram.config import TelegramIntegrationConfig
            from clud.telegram.server import TelegramServer

            # Load configuration
            self.config = TelegramIntegrationConfig.load(config_file=config_path)

            # Override port if provided
            if port is not None:
                self.config.web.port = port

            # Validate configuration
            validation_errors = self.config.validate()
            if validation_errors:
                logger.error(f"Telegram configuration errors: {validation_errors}")
                return False

            # Create telegram server
            self.telegram_server = TelegramServer(self.config)

            # Start in separate thread with its own event loop
            def run_telegram_service() -> None:
                """Run telegram service in its own thread."""
                import uvicorn

                # Create new event loop for this thread
                self.asyncio_loop = asyncio.new_event_loop()
                asyncio.set_event_loop(self.asyncio_loop)

                try:
                    # Start telegram server (bot + web)
                    self.asyncio_loop.run_until_complete(self.telegram_server.start())

                    # Run uvicorn server
                    if self.telegram_server.app:
                        uvicorn_config = uvicorn.Config(
                            self.telegram_server.app,
                            host=self.config.web.host,
                            port=self.config.web.port,
                            log_level=self.config.logging.level.lower(),
                        )
                        uvicorn_server = uvicorn.Server(uvicorn_config)
                        self.asyncio_loop.run_until_complete(uvicorn_server.serve())
                except Exception as e:
                    logger.error(f"Telegram service error: {e}", exc_info=True)
                finally:
                    # Cleanup
                    if self.telegram_server:
                        self.asyncio_loop.run_until_complete(self.telegram_server.stop())
                    self.asyncio_loop.close()
                    self.is_running = False

            # Start thread
            self.server_thread = threading.Thread(target=run_telegram_service, daemon=True)
            self.server_thread.start()
            self.is_running = True

            logger.info(f"Telegram service started on {self.config.web.host}:{self.config.web.port}")
            return True

        except Exception as e:
            logger.error(f"Failed to start telegram service: {e}", exc_info=True)
            return False

    def stop_service(self) -> bool:
        """Stop the telegram service.

        Returns:
            True if stopped successfully, False if not running
        """
        if not self.is_running:
            logger.warning("Telegram service not running")
            return False

        logger.info("Stopping telegram service...")

        try:
            # Signal the event loop to stop
            if self.asyncio_loop and self.telegram_server:
                # Schedule stop coroutine in the telegram service's event loop
                asyncio.run_coroutine_threadsafe(self.telegram_server.stop(), self.asyncio_loop)

            # Wait for thread to finish (with timeout)
            if self.server_thread:
                self.server_thread.join(timeout=5.0)

            self.is_running = False
            self.server_thread = None
            self.telegram_server = None
            self.asyncio_loop = None

            logger.info("Telegram service stopped")
            return True

        except Exception as e:
            logger.error(f"Error stopping telegram service: {e}", exc_info=True)
            return False
