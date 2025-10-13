"""Spinner utilities for clud CLI."""

import threading
import time
from types import TracebackType


class Spinner:
    """Simple text-based spinner for CLI operations."""

    def __init__(self, message: str = "Working...") -> None:
        self.message = message
        self._stop_event = threading.Event()
        self._thread: threading.Thread | None = None
        self._chars = "|/-\\"
        self._running = False

    def start(self) -> None:
        """Start the spinner."""
        if self._running:
            return
        self._running = True
        self._stop_event.clear()
        self._thread = threading.Thread(target=self._spin)
        self._thread.start()

    def stop(self) -> None:
        """Stop the spinner."""
        if not self._running:
            return
        self._running = False
        self._stop_event.set()
        if self._thread:
            self._thread.join()
        print()  # New line after spinner

    def _spin(self) -> None:
        """Internal method to animate the spinner."""
        i = 0
        while not self._stop_event.is_set():
            char = self._chars[i % len(self._chars)]
            print(f"\r{char} {self.message}", end="", flush=True)
            time.sleep(0.1)
            i += 1

    def __enter__(self) -> "Spinner":
        """Context manager entry."""
        self.start()
        return self

    def __exit__(self, exc_type: type[BaseException] | None, exc_val: BaseException | None, exc_tb: TracebackType | None) -> None:
        """Context manager exit."""
        self.stop()
