"""Subprocess runner for Telegram message processing.

This module handles running clud subprocesses for each incoming Telegram message.
It can use either direct subprocess calls or the MessageHandler API.
"""

import asyncio
import logging
import subprocess
import sys
from typing import Any

logger = logging.getLogger(__name__)


async def run_clud_with_message_api(message: str, chat_id: str, user_id: str, username: str, message_handler: Any) -> tuple[bool, str]:
    """Run clud via MessageHandler API (async version).

    Args:
        message: The message/prompt to send to clud
        chat_id: Telegram chat ID (used as session_id)
        user_id: Telegram user ID (used as client_id)
        username: Username for logging
        message_handler: MessageHandler instance

    Returns:
        Tuple of (success: bool, response_message: str)
    """
    try:
        from clud.api.models import ClientType, MessageRequest

        # Create MessageRequest from Telegram message
        request = MessageRequest(
            message=message,
            session_id=chat_id,  # Use chat_id as session_id for session persistence
            client_type=ClientType.TELEGRAM,
            client_id=user_id,
            metadata={"username": username},
        )

        logger.info(f"Processing message via API for chat {chat_id}: {message[:50]}...")

        # Forward to MessageHandler API
        response = await message_handler.handle_message(request)

        # Format response
        if response.error:
            logger.error(f"MessageHandler returned error: {response.error}")
            return False, f"❌ Error: {response.error}"

        # Build response message
        status_emoji = {"completed": "✅", "running": "⏳", "failed": "❌", "pending": "📝"}.get(response.status.value, "📝")

        response_text = f"{status_emoji} Message processed\nInstance: `{response.instance_id[:8]}...`"

        if response.message:
            response_text += f"\n\n{response.message}"

        logger.info(f"Message processed successfully via API, status: {response.status.value}")
        return True, response_text

    except Exception as e:
        logger.error(f"Error processing message via API: {e}", exc_info=True)
        return False, f"❌ Error: {str(e)}"


def run_clud_with_message(message: str, plain: bool = True, continue_flag: bool = False) -> int:
    """Run clud subprocess with a message via -p flag (legacy direct subprocess method).

    Args:
        message: The message/prompt to send to clud
        plain: If True, use --plain mode for raw text I/O
        continue_flag: If True, add --continue flag

    Returns:
        Exit code from clud subprocess
    """
    try:
        # Build command: python -m clud -p "<message>"
        cmd = [sys.executable, "-m", "clud", "-p", message]

        # Add plain mode flag if requested
        if plain:
            cmd.append("--plain")

        # Add continue flag if requested
        if continue_flag:
            cmd.insert(3, "--continue")

        logger.info(f"Running clud subprocess with message: {message[:50]}...")

        # Run subprocess with stdout/stderr going to terminal
        result = subprocess.run(
            cmd,
            check=False,  # Don't raise on non-zero exit
            capture_output=False,  # Let output go to terminal
        )
        logger.info(f"Subprocess completed with exit code {result.returncode}")
        return result.returncode

    except FileNotFoundError:
        logger.error("Error: Python interpreter not found.")
        print("Error: Python interpreter not found.", file=sys.stderr)
        return 1
    except Exception as e:
        logger.error(f"Error running clud subprocess: {e}")
        print(f"Error running clud subprocess: {e}", file=sys.stderr)
        return 1


async def run_telegram_message_loop_async(messenger: Any, message_handler: Any = None) -> int:
    """Run the main Telegram message processing loop (async version).

    This function:
    1. Starts listening for Telegram messages
    2. Waits for incoming messages
    3. For each message, either:
       - Uses MessageHandler API if provided (recommended)
       - Falls back to running clud -p in subprocess
    4. Checks for more pending messages and repeats

    Args:
        messenger: TelegramMessenger instance
        message_handler: Optional MessageHandler instance for API-based processing

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    logger.info("Starting Telegram message loop...")
    mode = "API" if message_handler else "subprocess"
    print(f"🤖 Telegram runner started ({mode} mode). Listening for messages...")
    print("Press Ctrl+C to stop.")

    try:
        # Start listening for Telegram updates
        await messenger.start_listening()

        # Main loop: wait for messages and process them
        while True:
            # Wait for next message (60 second timeout)
            message_text = await messenger.receive_message(timeout=60)

            if message_text:
                logger.info(f"Received message: {message_text[:50]}...")
                print(f"\n📨 Received message: {message_text[:80]}...")

                # Use MessageHandler API if available, otherwise fall back to subprocess
                if message_handler:
                    # Extract chat info (we need to get this from the messenger somehow)
                    chat_id = str(messenger.chat_id)
                    # For now, use chat_id as user_id and username
                    success, response = await run_clud_with_message_api(
                        message=message_text,
                        chat_id=chat_id,
                        user_id=chat_id,
                        username="telegram_user",
                        message_handler=message_handler,
                    )

                    if not success:
                        logger.warning(f"Message processing failed: {response}")
                        print(f"⚠️  {response}")
                    else:
                        logger.info("Message processed successfully via API")
                        print(f"✅ {response}")
                else:
                    # Legacy subprocess method
                    exit_code = run_clud_with_message(message_text, plain=True)

                    if exit_code != 0:
                        logger.warning(f"Message processing exited with code {exit_code}")
                        print(f"⚠️  Processing completed with exit code {exit_code}")
                    else:
                        logger.info("Message processed successfully")
                        print("✅ Message processed successfully")

                # Check if there are more messages in queue
                if not messenger.message_queue.empty():
                    logger.info("More messages pending in queue...")
                    print("📬 More messages pending...")
                    continue

            # No message received (timeout) - continue waiting
            logger.debug("No message received (timeout), continuing to listen...")

    except KeyboardInterrupt:
        logger.info("Runner interrupted by user")
        print("\n\n⏹️  Stopping Telegram runner...")
        return 0
    except Exception as e:
        logger.error(f"Error in message loop: {e}")
        print(f"Error in message loop: {e}", file=sys.stderr)
        return 1
    finally:
        # Stop listening for messages
        await messenger.stop_listening()
        logger.info("Telegram runner stopped")
        print("✓ Telegram runner stopped")

    return 0


def run_telegram_message_loop(messenger: Any, message_handler: Any = None) -> int:
    """Run the main Telegram message processing loop (sync wrapper).

    Args:
        messenger: TelegramMessenger instance
        message_handler: Optional MessageHandler instance for API-based processing

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        # Create new event loop
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

        # Run the async loop
        result = loop.run_until_complete(run_telegram_message_loop_async(messenger, message_handler))

        loop.close()
        return result

    except Exception as e:
        logger.error(f"Error running Telegram message loop: {e}")
        print(f"Error running Telegram message loop: {e}", file=sys.stderr)
        return 1
