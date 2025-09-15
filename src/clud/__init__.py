"""Claude in a Docker Box package."""

import os
import sys

# Set UTF-8 encoding for stdin, stdout, and stderr
if sys.platform == "win32":
    # On Windows, ensure UTF-8 encoding for all streams
    try:
        # Check if streams support reconfigure (pytest may replace with DontReadFromInput)
        if hasattr(sys.stdin, "reconfigure"):
            sys.stdin.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        if hasattr(sys.stdout, "reconfigure"):
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        if hasattr(sys.stderr, "reconfigure"):
            sys.stderr.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
    except (AttributeError, OSError):
        # Handle pytest or other environments that replace streams
        try:
            import codecs

            # Only reconfigure if streams have detach method
            if hasattr(sys.stdin, "detach"):
                sys.stdin = codecs.getreader("utf-8")(sys.stdin.detach())  # type: ignore[attr-defined]
            if hasattr(sys.stdout, "detach"):
                sys.stdout = codecs.getwriter("utf-8")(sys.stdout.detach())  # type: ignore[attr-defined]
            if hasattr(sys.stderr, "detach"):
                sys.stderr = codecs.getwriter("utf-8")(sys.stderr.detach())  # type: ignore[attr-defined]
        except (AttributeError, OSError):
            # In pytest or other test environments, streams may be replaced
            # Just continue without reconfiguration
            pass

# Set environment variable for subprocess UTF-8 handling
os.environ.setdefault("PYTHONIOENCODING", "utf-8")
