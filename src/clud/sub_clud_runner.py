"""Subprocess runner for Telegram message processing.

This module handles running clud subprocesses for each incoming Telegram message.
"""

import asyncio
import logging
import subprocess
import sys
from typing import Any

logger = logging.getLogger(__name__)


def run_clud_with_message(message: str, plain: bool = True, continue_flag: bool = False) -> int:
    """Run clud subprocess with a message via -p flag.

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


async def run_telegram_message_loop_async(messenger: Any) -> int:
    """Run the main Telegram message processing loop (async version).

    This function:
    1. Starts listening for Telegram messages
    2. Waits for incoming messages
    3. For each message, runs clud -p in subprocess
    4. Checks for more pending messages and repeats

    Args:
        messenger: TelegramMessenger instance

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    logger.info("Starting Telegram message loop...")
    print("🤖 Telegram runner started. Listening for messages...")
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

                # Run clud -p with the message
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


def run_telegram_message_loop(messenger: Any) -> int:
    """Run the main Telegram message processing loop (sync wrapper).

    Args:
        messenger: TelegramMessenger instance

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    try:
        # Create new event loop
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

        # Run the async loop
        result = loop.run_until_complete(run_telegram_message_loop_async(messenger))

        loop.close()
        return result

    except Exception as e:
        logger.error(f"Error running Telegram message loop: {e}")
        print(f"Error running Telegram message loop: {e}", file=sys.stderr)
        return 1
