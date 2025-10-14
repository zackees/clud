"""WebSocket handler for terminal sessions."""

import asyncio
import contextlib
import logging
import os
from typing import Any, cast

from fastapi import WebSocket

from .pty_manager import PTYManager

logger = logging.getLogger(__name__)


class TerminalHandler:
    """Handles WebSocket connections for terminal sessions."""

    def __init__(self, pty_manager: PTYManager) -> None:
        """Initialize terminal handler.

        Args:
            pty_manager: PTY manager instance
        """
        self.pty_manager = pty_manager

    async def handle_websocket(self, websocket: WebSocket, session_id: str) -> None:
        """Handle a terminal WebSocket connection.

        Args:
            websocket: WebSocket connection
            session_id: Terminal session ID
        """
        await websocket.accept()
        logger.info("Terminal WebSocket connected: session=%s, client=%s", session_id, websocket.client)

        # Output queue for async communication
        output_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()

        # Get the event loop for thread-safe operations
        loop = asyncio.get_running_loop()

        def on_output(data: bytes) -> None:
            """Callback for terminal output."""
            # Decode output
            try:
                text = data.decode("utf-8", errors="replace")
            except Exception:
                text = data.decode("latin-1", errors="replace")

            # Queue for async sending (thread-safe)
            asyncio.run_coroutine_threadsafe(output_queue.put({"type": "output", "data": text}), loop)

        def on_exit(code: int) -> None:
            """Callback for process exit."""
            asyncio.run_coroutine_threadsafe(output_queue.put({"type": "exit", "code": code}), loop)

        pty_session = None

        try:
            # Wait for init message
            data = await websocket.receive_json()
            if data.get("type") != "init":
                await websocket.send_json({"type": "error", "error": "Expected init message"})
                return

            # Create PTY session
            cwd = data.get("cwd", os.getcwd())
            cols = data.get("cols", 80)
            rows = data.get("rows", 24)

            # Validate cwd exists and is a directory
            if not os.path.isdir(cwd):
                logger.warning("Invalid cwd: %s, using current directory", cwd)
                cwd = os.getcwd()

            pty_session = self.pty_manager.create_session(
                session_id=session_id,
                cwd=cwd,
                cols=cols,
                rows=rows,
                on_output=on_output,
                on_exit=on_exit,
            )

            # Send acknowledgment
            await websocket.send_json({"type": "ready"})

            # Main loop: handle input and output
            while True:
                # Wait for either WebSocket message or output queue item
                receive_task = asyncio.create_task(websocket.receive_json())
                queue_task = asyncio.create_task(output_queue.get())

                done, pending = await asyncio.wait(
                    [receive_task, queue_task],
                    return_when=asyncio.FIRST_COMPLETED,
                )

                # Cancel pending tasks
                for task in pending:
                    task.cancel()
                    with contextlib.suppress(asyncio.CancelledError):
                        await task

                # Process completed task
                for task in done:
                    # Check if task was cancelled (e.g., when test runs out of messages)
                    if task.cancelled():
                        continue

                    try:
                        result = task.result()
                    except asyncio.CancelledError:
                        # Task was cancelled, continue to next
                        continue

                    # Check if this is output from queue
                    if task == queue_task:
                        # Output from PTY
                        await websocket.send_json(result)
                    else:
                        # Input from WebSocket
                        if not isinstance(result, dict):
                            continue

                        msg_dict = cast(dict[str, Any], result)
                        msg_type = msg_dict.get("type")

                        if msg_type == "input":
                            # Write to PTY
                            data_str = str(msg_dict.get("data", ""))
                            self.pty_manager.write_input(session_id, data_str.encode("utf-8"))

                        elif msg_type == "resize":
                            # Resize PTY
                            cols = int(msg_dict.get("cols", 80))
                            rows = int(msg_dict.get("rows", 24))
                            self.pty_manager.resize(session_id, cols, rows)

        except Exception as e:
            logger.exception("Error in terminal WebSocket handler: %s", e)
            with contextlib.suppress(Exception):
                await websocket.send_json({"type": "error", "error": str(e)})

        finally:
            # Clean up PTY session
            if pty_session:
                self.pty_manager.close_session(session_id)

            logger.info("Terminal WebSocket disconnected: session=%s", session_id)
