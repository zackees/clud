"""End-to-end check that ``clud`` stamps the console window title.

PR #78 added a one-shot ``SetConsoleTitleW`` call near the top of
``main()`` so a Windows Terminal / cmd.exe window running ``clud`` is
identifiable at a glance. PR #86 added a background keeper plus a PTY-mode
OSC stripper to defend that title against children that overwrite it.

The cheapest assertion is the **first half**: did the one-shot stamp
land at all? On Windows we read the title back with ``GetConsoleTitleW``
via ctypes after ``clud --version`` exits — the title persists in the
host conhost / Windows Terminal session. On POSIX the design is a
deliberate no-op (terminal-title management out of scope per the
originating issue), so the asserted contract is "clud must not emit any
OSC 0/2 escape sequence to stdio" — anything else would silently drift
the host shell's title and surprise users.

The keeper-thread defense itself isn't checked here — proving it would
need ``clud`` to stay alive while a sibling overwrites the title, which
requires a running ``claude``/``codex`` backend. The Rust unit tests in
``console_title.rs`` cover the keeper's invariants (cell population,
idempotent spawn) and the OSC stripper's byte-level behavior.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

# `tests/conftest.py` puts `tests/` on sys.path automatically; reuse the
# locally-built CLUD binary that test_hello.py already arranges. This
# avoids re-running the cargo build a second time per pytest session.
_TESTS_DIR = Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

from test_hello import CLUD  # type: ignore[import-not-found]  # noqa: E402


def _run_clud_in_dir(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    """Run the locally-built clud with a controlled cwd.

    Why copy the binary into ``cwd``: the title formatter uses the
    basename of the *process* cwd, so the cleanest way to assert a
    deterministic title is to set a known cwd. Copying the binary
    avoids PATH lookups entirely.
    """
    source = Path(CLUD)
    launch = cwd / source.name
    shutil.copy2(source, launch)
    return subprocess.run(
        [str(launch), *args],
        capture_output=True,
        text=True,
        timeout=10,
        cwd=str(cwd),
    )


# ─── Windows: read the title back via GetConsoleTitleW ────────────────────


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only console-title API")
def test_clud_stamps_console_title_on_windows() -> None:
    """After ``clud`` exits, ``GetConsoleTitleW`` reports the stamped title.

    GetConsoleTitleW returns whatever title the *console* (host conhost
    / Windows Terminal) currently shows — and on Windows the title
    persists across child-process exit until something else overwrites
    it. So launching clud in the test runner's own console, then
    reading the title, observes the side effect of clud's startup.

    Subtlety: the title format is ``clud <basename(cwd)>``. We pick a
    deterministic basename by spawning clud from a fresh tempdir whose
    directory we name ourselves, then assert the exact string.
    """
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.GetConsoleTitleW.argtypes = [wintypes.LPWSTR, wintypes.DWORD]
    k32.GetConsoleTitleW.restype = wintypes.DWORD
    k32.SetConsoleTitleW.argtypes = [wintypes.LPCWSTR]
    k32.SetConsoleTitleW.restype = wintypes.BOOL

    def get_title() -> str:
        buf = ctypes.create_unicode_buffer(1024)
        k32.GetConsoleTitleW(buf, 1024)
        return buf.value

    sentinel = "clud-test-title-sentinel-xyz"
    original = get_title()
    try:
        # Stamp a known sentinel first so a passing assertion can't be
        # explained by the title coincidentally already being correct.
        k32.SetConsoleTitleW(sentinel)
        assert get_title() == sentinel, (
            "test setup failed: SetConsoleTitleW didn't take effect — "
            "is this running without a real console (CI / nested shell)?"
        )

        with tempfile.TemporaryDirectory(prefix="clud-title-test-") as tmp:
            cwd = Path(tmp)
            # The cwd basename is what clud will append to the title.
            # tempfile picks a unique basename starting with our prefix.
            expected = f"clud {cwd.name}"

            result = _run_clud_in_dir("--version", cwd=cwd)
            assert result.returncode == 0, (
                f"clud --version failed: stdout={result.stdout!r} "
                f"stderr={result.stderr!r}"
            )

            # Title-set is synchronous (SetConsoleTitleW is a single
            # syscall), but yield once to let conhost paint the change
            # before reading.
            time.sleep(0.05)
            actual = get_title()

        assert actual == expected, (
            f"console title not stamped: expected {expected!r}, got {actual!r}"
        )
    finally:
        # Always restore the user's original title — leaving "clud …"
        # behind would leak into the developer's terminal session.
        k32.SetConsoleTitleW(original)


# ─── POSIX: contract test — clud must NOT emit OSC 0/2 ────────────────────


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="POSIX-only contract — Windows uses SetConsoleTitleW, not OSC",
)
def test_clud_does_not_emit_osc_title_on_posix() -> None:
    """On POSIX ``set_title`` is a documented no-op, so clud must not
    emit any OSC 0/2 (window-title) escape sequence to stdio.

    Why this is the right contract test: querying the live terminal
    title on POSIX would require a cooperating terminal that responds
    to ``ESC ] 21 t`` (xterm-class) — most CI runners don't have a TTY
    at all, let alone one with title-reporting enabled. But verifying
    that clud's bytes don't *contain* an OSC title-set is platform-
    independent and catches the regression we'd actually care about
    (a hypothetical future change that started writing OSC 0/2 to
    stdout on POSIX, silently mutating the user's shell title).
    """
    with tempfile.TemporaryDirectory(prefix="clud-title-test-") as tmp:
        result = _run_clud_in_dir("--version", cwd=Path(tmp))
    assert result.returncode == 0

    blob = (result.stdout + result.stderr).encode("utf-8", errors="replace")
    # OSC 0; (icon + window title) and OSC 2; (window title only). The
    # ESC byte is 0x1B, ']' is 0x5D, then the digit and ';'.
    assert b"\x1b]0;" not in blob, (
        "clud emitted OSC 0 (set icon+window title) — POSIX path "
        "should be a no-op."
    )
    assert b"\x1b]2;" not in blob, (
        "clud emitted OSC 2 (set window title) — POSIX path should "
        "be a no-op."
    )
