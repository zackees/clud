"""Cross-platform subprocess-launch helpers shared by tests.

Lives outside ``conftest.py`` so it can be imported as a regular module from
both the integration-test conftest *and* lightweight unit tests that exist
purely to assert the helper's contract on each platform — see issue #55.

Keeping the helper here (rather than inline in ``conftest.py``) means a
single source of truth for the Windows ``CREATE_NO_WINDOW`` value the suite
applies to spawned subprocesses, and lets POSIX CI cover the no-op branch
without needing the integration harness to be enabled.
"""

from __future__ import annotations

import subprocess
import sys

# Documented value of the Win32 ``CREATE_NO_WINDOW`` process-creation flag.
# We hard-code it here (rather than only relying on ``subprocess.CREATE_NO_WINDOW``)
# so the unit test can assert the *exact* bit pattern even when the test is
# running off-Windows where the attribute does not exist.
CREATE_NO_WINDOW: int = 0x0800_0000


def windows_no_window_flags() -> dict[str, int]:
    """Return ``{"creationflags": CREATE_NO_WINDOW}`` on Windows, ``{}`` else.

    Use this helper when launching a piped, non-interactive child process
    from the test suite (or from any harness code that wants to suppress
    Windows' default conhost allocation). On POSIX the returned dict is
    empty, so spreading ``**windows_no_window_flags()`` into a
    ``subprocess.Popen``/``subprocess.run`` call is a portable no-op.

    See issue #55: every subprocess the integration suite spawns on Windows
    (clud, mock agents, daemon helpers, attach clients) inherits Windows'
    default of allocating a fresh console window — each one a visible flash
    that steals focus from the developer's editor during a ``pytest`` run.
    Setting CREATE_NO_WINDOW on the parent's CreateProcess call suppresses
    the allocation for piped/non-interactive children without changing any
    other launch semantics.
    """
    if sys.platform != "win32":
        return {}
    # ``getattr`` keeps the helper safe on hypothetical Python builds where
    # ``subprocess.CREATE_NO_WINDOW`` was not exposed; the documented bit
    # pattern is the source of truth.
    return {"creationflags": getattr(subprocess, "CREATE_NO_WINDOW", CREATE_NO_WINDOW)}


def add_windows_create_no_window(kwargs: dict) -> None:
    """OR ``CREATE_NO_WINDOW`` into ``kwargs["creationflags"]`` on Windows.

    The OR is deliberate: callers may already have set
    ``CREATE_NEW_PROCESS_GROUP`` (e.g. tests that send ``CTRL_BREAK_EVENT``
    to a child). The two flags are independent bits and compose without
    affecting Ctrl+Break delivery or process-group semantics.

    No-op on non-Windows platforms.
    """
    if sys.platform != "win32":
        return
    creationflags = kwargs.get("creationflags", 0)
    if not isinstance(creationflags, int):
        creationflags = 0
    kwargs["creationflags"] = creationflags | getattr(
        subprocess, "CREATE_NO_WINDOW", CREATE_NO_WINDOW
    )
