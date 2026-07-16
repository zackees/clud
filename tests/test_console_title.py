"""End-to-end checks for Clud's console-title contract.

PR #78 added a one-shot ``SetConsoleTitleW`` call near the top of ``main()``
so a Windows Terminal / cmd.exe window running ``clud`` is identifiable at a
glance. PR #86 added a background keeper plus a PTY-mode OSC stripper to
defend that title against children that overwrite it.

On Windows the assertion is "did the one-shot stamp land at all?". The title
is read back from inside a **dedicated console** the test owns (see
``_run_clud_in_isolated_console``) rather than the test runner's own console.
The earlier version of this test read the shared runner console and therefore
had to skip itself entirely under ``GITHUB_ACTIONS``; owning the console is
what lets the assertion actually run in CI.

On POSIX the design is a deliberate no-op (terminal-title management is out of
scope per the originating issue), so the asserted contract is "clud must not
emit any OSC 0/2 escape sequence to stdio" — anything else would silently
drift the host shell's title and surprise users.

The keeper-thread defense itself isn't checked here — proving it would need
``clud`` to stay alive while a sibling overwrites the title, which requires a
running ``claude``/``codex`` backend. The Rust unit tests in
``console_title.rs`` cover the keeper's invariants (cell population,
idempotent spawn) and the OSC stripper's byte-level behavior.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

_TESTS_DIR = Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

from test_hello import CLUD, copied_clud_env  # type: ignore[import-not-found]  # noqa: E402


def _run_clud_in_dir(*args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    """Run the locally-built Clud binary with a controlled cwd."""
    source = Path(CLUD)
    launch = cwd / source.name
    shutil.copy2(source, launch)
    return subprocess.run(
        [str(launch), *args],
        capture_output=True,
        text=True,
        timeout=10,
        cwd=str(cwd),
        env=copied_clud_env(source),
    )


def _run_clud_in_isolated_console(*args: str, cwd: Path) -> str:
    """Run Clud in an owned console and report its observed title.

    A Windows console title is mutable state shared by every process attached
    to that console. A dedicated PowerShell console prevents unrelated build
    spinners from racing this end-to-end assertion.

    The probe polls rather than reading once because the stamp lands somewhere
    inside clud's startup, not at spawn. It re-reads the title *after*
    observing process exit: a short-lived command (``--dry-run``) can set the
    title and exit between two polls, and a Windows console title outlives the
    process that set it, so the post-exit read is what makes the fast-exit case
    deterministic instead of a flake.
    """
    source = Path(CLUD)
    launch = cwd / source.name
    shutil.copy2(source, launch)
    result_path = cwd / "console-title-result.json"
    script_path = cwd / "console-title-probe.ps1"
    script_path.write_text(
        """$ErrorActionPreference = 'Stop'
$arguments = $env:CLUD_TITLE_TEST_ARGS | ConvertFrom-Json
$process = Start-Process -FilePath $env:CLUD_TITLE_TEST_EXE `
    -ArgumentList $arguments -WorkingDirectory $env:CLUD_TITLE_TEST_CWD `
    -NoNewWindow -PassThru
$title = [Console]::Title
while ($true) {
    $observed = [Console]::Title
    if ($observed -like 'clud *') {
        $title = $observed
        break
    }
    $process.Refresh()
    if ($process.HasExited) {
        # The title can be stamped between the poll above and this exit
        # check. A console title outlives the process that set it, so one
        # final read after exit closes that race instead of reporting the
        # pre-launch title.
        $observed = [Console]::Title
        if ($observed -like 'clud *') {
            $title = $observed
        }
        break
    }
    Start-Sleep -Milliseconds 10
}
$process.WaitForExit()
[PSCustomObject]@{
    title = $title
} | ConvertTo-Json -Compress | Set-Content -LiteralPath $env:CLUD_TITLE_TEST_RESULT -Encoding utf8
""",
        encoding="utf-8",
    )
    env = copied_clud_env(source)
    env.update(
        {
            "CLUD_TITLE_TEST_EXE": str(launch),
            "CLUD_TITLE_TEST_ARGS": json.dumps(args),
            "CLUD_TITLE_TEST_CWD": str(cwd),
            "CLUD_TITLE_TEST_RESULT": str(result_path),
        }
    )
    completed = subprocess.run(
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(script_path)],
        capture_output=True,
        text=True,
        timeout=20,
        cwd=str(cwd),
        env=env,
        creationflags=subprocess.CREATE_NEW_CONSOLE,
    )
    assert completed.returncode == 0, (
        f"isolated PowerShell probe failed: stdout={completed.stdout!r} stderr={completed.stderr!r}"
    )
    observed = json.loads(result_path.read_text(encoding="utf-8-sig"))
    return str(observed["title"])


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only console-title API")
def test_clud_stamps_console_title_on_windows() -> None:
    """A dedicated child console reports Clud's expected startup title."""
    with tempfile.TemporaryDirectory(prefix="clud-title-test-") as tmp:
        cwd = Path(tmp)
        expected = f"clud {cwd.name}"
        actual = _run_clud_in_isolated_console("--dry-run", "--codex", "-p", "hello", cwd=cwd)

    assert actual == expected, f"console title not stamped: expected {expected!r}, got {actual!r}"


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="POSIX-only contract — Windows uses SetConsoleTitleW, not OSC",
)
def test_clud_does_not_emit_osc_title_on_posix() -> None:
    """The POSIX title path is a no-op and must emit no OSC title sequence.

    Querying the live terminal title on POSIX would require a cooperating
    terminal that answers ``ESC ] 21 t`` (xterm-class); most CI runners have no
    TTY at all. Asserting that clud's bytes don't *contain* an OSC title-set is
    platform-independent and catches the regression we'd actually care about —
    a future change that starts writing OSC 0/2 to stdout and silently mutates
    the user's shell title.
    """
    with tempfile.TemporaryDirectory(prefix="clud-title-test-") as tmp:
        result = _run_clud_in_dir("--version", cwd=Path(tmp))
    assert result.returncode == 0, (
        f"clud --version failed: stdout={result.stdout!r} stderr={result.stderr!r}"
    )

    # OSC 0; (icon + window title) and OSC 2; (window title only). The ESC byte
    # is 0x1B, ']' is 0x5D, then the digit and ';'.
    blob = (result.stdout + result.stderr).encode("utf-8", errors="replace")
    assert b"\x1b]0;" not in blob, (
        "clud emitted OSC 0 (set icon+window title) — POSIX path should be a no-op."
    )
    assert b"\x1b]2;" not in blob, (
        "clud emitted OSC 2 (set window title) — POSIX path should be a no-op."
    )
