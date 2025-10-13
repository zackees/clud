"""Claude Code in YOLO mode package."""

import os
import sys
from typing import Any, cast

# Set UTF-8 encoding for stdin, stdout, and stderr
# Skip this during testing to avoid conflicts with pytest's output capture
if sys.platform == "win32" and "pytest" not in sys.modules:
    # On Windows, ensure UTF-8 encoding for all streams
    try:
        if hasattr(sys.stdin, "reconfigure"):
            sys.stdin.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        if hasattr(sys.stdout, "reconfigure"):
            sys.stdout.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        if hasattr(sys.stderr, "reconfigure"):
            sys.stderr.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
    except AttributeError:
        # Python < 3.7 fallback
        import codecs

        if hasattr(sys.stdin, "detach"):
            sys.stdin = cast(Any, codecs.getreader("utf-8")(sys.stdin.detach()))  # type: ignore[attr-defined]
        if hasattr(sys.stdout, "detach"):
            sys.stdout = cast(Any, codecs.getwriter("utf-8")(sys.stdout.detach()))  # type: ignore[attr-defined]
        if hasattr(sys.stderr, "detach"):
            sys.stderr = cast(Any, codecs.getwriter("utf-8")(sys.stderr.detach()))  # type: ignore[attr-defined]

# Set environment variable for subprocess UTF-8 handling
os.environ.setdefault("PYTHONIOENCODING", "utf-8")
