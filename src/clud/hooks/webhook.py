"""Webhook hook handler for sending events to HTTP endpoints.

This module provides a hook handler that sends execution events
to configured webhook URLs, enabling integration with external services.
"""

import asyncio
import logging
from typing import Any

from clud.hooks import HookContext

logger = logging.getLogger(__name__)


class WebhookHookHandler:
    """Hook handler that sends events to HTTP webhooks.

    This handler makes HTTP POST requests to a configured webhook URL
    with event data in JSON format.
    """

    def __init__(
        self,
        webhook_url: str,
        secret: str | None = None,
        timeout: float = 10.0,
        retry_count: int = 3,
    ) -> None:
        """Initialize the webhook hook handler.

        Args:
            webhook_url: URL to send webhook notifications to
            secret: Optional secret for webhook authentication
            timeout: HTTP request timeout in seconds (default: 10.0)
            retry_count: Number of retries on failure (default: 3)
        """
        self._webhook_url = webhook_url
        self._secret = secret
        self._timeout = timeout
        self._retry_count = retry_count

    async def handle(self, context: HookContext) -> None:
        """Handle a hook event and forward to webhook.

        Args:
            context: The hook context containing event information
        """
        try:
            # Prepare payload
            payload = self._prepare_payload(context)

            # Send to webhook with retries
            await self._send_with_retry(payload)

        except Exception as e:
            logger.error(f"Error handling webhook hook event {context.event.value}: {e}", exc_info=True)

    def _prepare_payload(self, context: HookContext) -> dict[str, Any]:
        """Prepare webhook payload from context.

        Args:
            context: The hook context

        Returns:
            Dictionary payload for webhook
        """
        payload: dict[str, Any] = {
            "event": context.event.value,
            "instance_id": context.instance_id,
            "session_id": context.session_id,
            "client_type": context.client_type,
            "client_id": context.client_id,
            "timestamp": context.timestamp.isoformat(),
        }

        # Add optional fields
        if context.message:
            payload["message"] = context.message

        if context.output:
            payload["output"] = context.output

        if context.error:
            payload["error"] = context.error

        if context.metadata:
            payload["metadata"] = context.metadata

        return payload

    async def _send_with_retry(self, payload: dict[str, Any]) -> None:
        """Send webhook with retry logic.

        Args:
            payload: The payload to send
        """
        last_error = None

        for attempt in range(self._retry_count):
            try:
                await self._send_webhook(payload)
                return  # Success
            except Exception as e:
                last_error = e
                if attempt < self._retry_count - 1:
                    # Wait before retry with exponential backoff
                    wait_time = 2**attempt
                    logger.warning(f"Webhook send failed (attempt {attempt + 1}/{self._retry_count}), retrying in {wait_time}s: {e}")
                    await asyncio.sleep(wait_time)
                else:
                    logger.error(f"Webhook send failed after {self._retry_count} attempts: {e}", exc_info=True)

        if last_error:
            raise last_error

    async def _send_webhook(self, payload: dict[str, Any]) -> None:
        """Send webhook HTTP request.

        Args:
            payload: The payload to send
        """
        try:
            import httpx

            headers = {"Content-Type": "application/json"}

            # Add authentication if secret provided
            if self._secret:
                headers["X-Webhook-Secret"] = self._secret

            async with httpx.AsyncClient(timeout=self._timeout) as client:
                response = await client.post(
                    self._webhook_url,
                    json=payload,
                    headers=headers,
                )

                # Raise for non-2xx status codes
                response.raise_for_status()

                logger.debug(f"Webhook sent successfully to {self._webhook_url}")

        except Exception as e:
            logger.error(f"Failed to send webhook to {self._webhook_url}: {e}")
            raise


__all__ = ["WebhookHookHandler"]
