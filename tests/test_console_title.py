"""End-to-end checks for Clud's console-title contract."""

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
    """The POSIX title path is a no-op and must emit no OSC title sequence."""
    with tempfile.TemporaryDirectory(prefix="clud-title-test-") as tmp:
        result = _run_clud_in_dir("--version", cwd=Path(tmp))
    assert result.returncode == 0

    blob = (result.stdout + result.stderr).encode("utf-8", errors="replace")
    assert b"\x1b]0;" not in blob
    assert b"\x1b]2;" not in blob
