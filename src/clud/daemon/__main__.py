"""Entry point for running the multi-terminal UI as a module (python -m clud.daemon).

This module is invoked when the UI is spawned as a separate process via `clud --ui`.
Signal handlers are set up here to handle Ctrl+C gracefully without propagating
to the parent process.
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import logging.handlers
import os
import signal
import sys
from pathlib import Path
from types import FrameType

logger = logging.getLogger(__name__)

# Global flag for graceful shutdown
_shutdown_requested = False


def _signal_handler(signum: int, frame: FrameType | None) -> None:
    """Handle shutdown signals gracefully.

    This handler is called when the daemon receives SIGINT (Ctrl+C) or SIGTERM.
    It sets a global flag to request graceful shutdown instead of raising
    KeyboardInterrupt.

    Args:
        signum: Signal number received
        frame: Current stack frame (unused)
    """
    global _shutdown_requested
    del frame  # unused
    signal_name = signal.Signals(signum).name if signum in signal.Signals._value2member_map_ else str(signum)
    logger.info("Received signal %s, requesting graceful shutdown...", signal_name)
    _shutdown_requested = True


def _setup_signal_handlers() -> None:
    """Set up signal handlers for graceful shutdown.

    This MUST be called at the start of the daemon process to ensure
    Ctrl+C is handled by the daemon, not propagated to the parent.
    """
    if sys.platform != "win32":
        # Unix signals
        signal.signal(signal.SIGTERM, _signal_handler)
        signal.signal(signal.SIGINT, _signal_handler)
        signal.signal(signal.SIGHUP, _signal_handler)
    else:
        # Windows signals (limited support)
        signal.signal(signal.SIGINT, _signal_handler)
        signal.signal(signal.SIGBREAK, _signal_handler)

    logger.debug("Signal handlers installed")


async def _run_daemon(num_terminals: int, port: int | None = None) -> int:
    """Run the daemon with signal-aware shutdown.

    Args:
        num_terminals: Number of terminals to create
        port: Optional specific port to use

    Returns:
        Exit code (0 for success, non-zero for error)
    """
    global _shutdown_requested

    from clud.daemon.playwright_daemon import PlaywrightDaemon

    daemon = PlaywrightDaemon(num_terminals=num_terminals)

    try:
        info = await daemon.start(port=port)
        logger.info("Daemon started (PID: %d, Port: %d)", info.pid, info.port)

        # Wait for shutdown signal or browser close
        while not _shutdown_requested:
            # Check if browser is still open
            if daemon.is_closed():
                logger.info("Browser closed by user")
                break

            # Small sleep to avoid busy-waiting
            await asyncio.sleep(0.1)

        logger.info("Shutting down daemon...")
        return 0

    except Exception as e:
        logger.exception("Daemon error: %s", e)
        return 1

    finally:
        # Always clean up
        await daemon.close()
        logger.info("Daemon shutdown complete")


def main() -> None:
    """Main entry point for the daemon module."""
    # Parse arguments
    parser = argparse.ArgumentParser(description="Multi-terminal daemon")
    parser.add_argument("command", choices=["run"], help="Command to run")
    parser.add_argument("--num-terminals", type=int, default=4, help="Number of terminals")
    parser.add_argument("--port", type=int, default=None, help="Port to use")
    args = parser.parse_args()

    if args.command != "run":
        print(f"Unknown command: {args.command}")
        sys.exit(1)

    # Set up logging
    log_dir = Path.home() / ".clud" / "logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    log_file = log_dir / "daemon.log"

    # Use rotating file handler
    file_handler = logging.handlers.RotatingFileHandler(
        log_file,
        maxBytes=10 * 1024 * 1024,  # 10MB
        backupCount=5,
        encoding="utf-8",
    )
    file_handler.setFormatter(logging.Formatter("%(asctime)s [%(levelname)s] %(name)s: %(message)s"))

    # Configure root logger
    log_handlers: list[logging.Handler] = [file_handler]

    # Add console handler on non-Windows (Windows pythonw.exe has no console)
    if sys.platform != "win32":
        console_handler = logging.StreamHandler()
        console_handler.setFormatter(logging.Formatter("%(asctime)s [%(levelname)s] %(message)s"))
        log_handlers.append(console_handler)

    logging.basicConfig(level=logging.INFO, handlers=log_handlers)

    # Set up signal handlers BEFORE starting async loop
    # This ensures Ctrl+C is caught by our handler, not asyncio
    _setup_signal_handlers()

    logger.info("=" * 60)
    logger.info("Multi-terminal daemon starting")
    logger.info("PID: %d", os.getpid())
    logger.info("Terminals: %d", args.num_terminals)
    logger.info("=" * 60)

    # Run the daemon
    try:
        exit_code = asyncio.run(_run_daemon(args.num_terminals, args.port))
        sys.exit(exit_code)
    except Exception as e:
        logger.exception("Fatal error: %s", e)
        sys.exit(1)


if __name__ == "__main__":
    main()
