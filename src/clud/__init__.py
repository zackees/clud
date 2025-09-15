"""Claude in a Docker Box package."""

import os
import sys

# Set UTF-8 encoding for stdin, stdout, and stderr
# Skip this during testing to avoid conflicts with pytest's output capture
if sys.platform == "win32" and "pytest" not in sys.modules:
    # On Windows, ensure UTF-8 encoding for all streams
    try:
        # Type: ignore because reconfigure might not be available on all TextIO implementations
        sys.stdin.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        sys.stderr.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
    except (AttributeError, OSError):
        # Handle cases where reconfigure is not available or streams don't support it
        # This includes Python < 3.7 and test environments like pytest
        try:
            import codecs

            # Only reconfigure streams that have detach method (real file streams)
            if hasattr(sys.stdin, "detach"):
                # Type: ignore because detach might not be available on all TextIO implementations
                sys.stdin = codecs.getreader("utf-8")(sys.stdin.detach())  # type: ignore[attr-defined]
            if hasattr(sys.stdout, "detach"):
                sys.stdout = codecs.getwriter("utf-8")(sys.stdout.detach())  # type: ignore[attr-defined]
            if hasattr(sys.stderr, "detach"):
                sys.stderr = codecs.getwriter("utf-8")(sys.stderr.detach())  # type: ignore[attr-defined]
        except (AttributeError, OSError):
            # If all else fails, just continue - this might be a test environment
            pass

# Set environment variable for subprocess UTF-8 handling
os.environ.setdefault("PYTHONIOENCODING", "utf-8")
