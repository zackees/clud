"""Unit tests for the Windows ``CREATE_NO_WINDOW`` test-suite helpers.

Issue #55: the integration suite spawned a small flotilla of clud / mock /
daemon-helper subprocesses, and on Windows each one popped a console
window that stole focus from the developer's editor. The fix is a small
helper (``windows_no_window_flags`` / ``add_windows_create_no_window``)
that the suite threads into every spawn, gated on ``sys.platform ==
"win32"``.

These tests cover the helper's contract in isolation so it can be
validated on POSIX CI without enabling the integration harness — and so a
regression that flips the bit pattern (or turns the POSIX no-op into a
no-op-but-with-a-key) gets caught at the cheapest layer.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

# `tests/conftest.py` has no sibling `tests/__init__.py`, so pytest puts
# `tests/` on `sys.path` automatically; this import is a backstop in case
# the test is collected via a different mechanism.
_TESTS_DIR = Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

from _subprocess_helpers import (  # type: ignore[import-not-found]  # noqa: E402
    CREATE_NO_WINDOW,
    add_windows_create_no_window,
    windows_no_window_flags,
)


# ---- windows_no_window_flags ------------------------------------------------


def test_create_no_window_constant_matches_win32_documentation() -> None:
    """The bit pattern is documented as 0x0800_0000 in winbase.h.

    Anchoring it as a literal here means a typo'd refactor that quietly
    flips a digit fails the build instead of silently allocating consoles
    again on Windows runners.
    """
    assert CREATE_NO_WINDOW == 0x0800_0000


def test_windows_no_window_flags_returns_empty_dict_on_posix() -> None:
    """On macOS / Linux the helper must be a true no-op.

    A non-empty dict here would either crash ``subprocess.Popen`` (which
    rejects ``creationflags`` on POSIX) or silently shadow a future
    POSIX-only kwarg the caller wanted to thread through. The helper's
    whole point is to be safely spreadable cross-platform.
    """
    if sys.platform == "win32":
        # On Windows we expect the OPPOSITE branch — see the dedicated
        # Windows-only test below. Skipping here keeps each test single-
        # purpose and easy to read in CI logs.
        import pytest

        pytest.skip("Windows-only: covered by test_windows_no_window_flags_returns_create_no_window_on_windows")
    assert windows_no_window_flags() == {}


def test_windows_no_window_flags_returns_create_no_window_on_windows() -> None:
    """On Windows the helper returns exactly the documented flag bit.

    We deliberately compare against the literal 0x0800_0000 (rather than
    just ``subprocess.CREATE_NO_WINDOW``) so a future Python that exposed
    a different value under the same name would still fail the test.
    """
    if sys.platform != "win32":
        import pytest

        pytest.skip("POSIX-only: covered by test_windows_no_window_flags_returns_empty_dict_on_posix")
    flags = windows_no_window_flags()
    assert set(flags.keys()) == {"creationflags"}
    assert flags["creationflags"] == 0x0800_0000
    # The stdlib constant should agree (sanity check on the host Python).
    assert flags["creationflags"] == subprocess.CREATE_NO_WINDOW


def test_windows_no_window_flags_can_be_spread_into_subprocess_kwargs() -> None:
    """``**windows_no_window_flags()`` must compose with arbitrary kwargs.

    Real call sites look like ``subprocess.Popen(..., **other,
    **windows_no_window_flags())``. The helper must never fight existing
    keys — its empty-dict POSIX branch must leave every other key alone.
    """
    base = {"text": True, "stdin": subprocess.PIPE}
    merged = {**base, **windows_no_window_flags()}
    assert merged["text"] is True
    assert merged["stdin"] is subprocess.PIPE
    if sys.platform == "win32":
        assert merged["creationflags"] == 0x0800_0000
    else:
        assert "creationflags" not in merged


# ---- add_windows_create_no_window ------------------------------------------


def test_add_windows_create_no_window_is_noop_on_posix() -> None:
    """The mutator helper must not touch kwargs on POSIX."""
    if sys.platform == "win32":
        import pytest

        pytest.skip("Windows-only path covered separately")
    kwargs: dict = {"shell": False}
    add_windows_create_no_window(kwargs)
    assert kwargs == {"shell": False}


def test_add_windows_create_no_window_sets_flag_on_windows() -> None:
    """When no creationflags is set, the helper installs CREATE_NO_WINDOW."""
    if sys.platform != "win32":
        import pytest

        pytest.skip("Windows-only behavior")
    kwargs: dict = {}
    add_windows_create_no_window(kwargs)
    assert kwargs == {"creationflags": 0x0800_0000}


def test_add_windows_create_no_window_ors_with_existing_flag() -> None:
    """Composing with CREATE_NEW_PROCESS_GROUP is a hard requirement.

    Some integration tests deliver Ctrl+Break to children and need
    ``CREATE_NEW_PROCESS_GROUP`` (0x0000_0200) on the spawn. The two
    flags are independent bits and must compose — if the helper
    overwrote ``creationflags`` instead of OR'ing, those tests would
    silently lose their process-group semantics and the Ctrl+Break
    delivery would land on the test runner instead.
    """
    if sys.platform != "win32":
        import pytest

        pytest.skip("Windows-only behavior")
    create_new_process_group = 0x0000_0200
    kwargs: dict = {"creationflags": create_new_process_group}
    add_windows_create_no_window(kwargs)
    expected = create_new_process_group | 0x0800_0000
    assert kwargs["creationflags"] == expected
    # Re-running the helper is idempotent — OR'ing the same bit twice is
    # the same bit pattern.
    add_windows_create_no_window(kwargs)
    assert kwargs["creationflags"] == expected


def test_add_windows_create_no_window_recovers_from_non_int_creationflags() -> None:
    """Defensive: if a caller stuffed a non-int into creationflags the
    helper resets to a clean bitfield instead of raising. This matches
    the conftest's existing tolerance — better to suppress the popup
    than abort the whole pytest session over a stray ``None``."""
    if sys.platform != "win32":
        import pytest

        pytest.skip("Windows-only behavior")
    kwargs: dict = {"creationflags": None}
    add_windows_create_no_window(kwargs)
    assert kwargs["creationflags"] == 0x0800_0000


def test_add_windows_create_no_window_does_not_strip_other_kwargs() -> None:
    """The helper must touch only ``creationflags``, leaving every other
    Popen/run kwarg untouched (otherwise threading it through real call
    sites would silently drop ``stdin`` / ``cwd`` / ``env`` etc.)."""
    if sys.platform != "win32":
        import pytest

        pytest.skip("Windows-only behavior")
    kwargs: dict = {
        "stdin": subprocess.PIPE,
        "cwd": "/tmp",
        "env": {"FOO": "bar"},
        "text": True,
    }
    add_windows_create_no_window(kwargs)
    assert kwargs["stdin"] is subprocess.PIPE
    assert kwargs["cwd"] == "/tmp"
    assert kwargs["env"] == {"FOO": "bar"}
    assert kwargs["text"] is True
    assert kwargs["creationflags"] == 0x0800_0000
