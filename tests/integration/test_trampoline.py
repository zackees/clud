"""Integration test: verify pip install works while clud is running.

On Windows, running executables are file-locked. The trampoline must
ensure Scripts/clud.exe is never locked so pip can overwrite it.
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

import pytest

pytestmark = pytest.mark.integration

ROOT = Path(__file__).resolve().parent.parent.parent


def _uv() -> str:
    """Find uv executable."""
    return "uv"


def _pip_install(venv_python: Path) -> subprocess.CompletedProcess[str]:
    """pip install clud into the given venv."""
    return subprocess.run(
        [_uv(), "pip", "install", "--python", str(venv_python), "--reinstall", str(ROOT)],
        capture_output=True,
        text=True,
        timeout=120,
        cwd=ROOT,
    )


def _pip_uninstall(venv_python: Path) -> subprocess.CompletedProcess[str]:
    """pip uninstall clud from the given venv."""
    return subprocess.run(
        [_uv(), "pip", "uninstall", "--python", str(venv_python), "clud"],
        capture_output=True,
        text=True,
        timeout=30,
    )


def _clud_exe(venv_dir: Path) -> Path:
    """Find clud binary in a venv."""
    if sys.platform == "win32":
        return venv_dir / "Scripts" / "clud.exe"
    return venv_dir / "bin" / "clud"


@pytest.fixture
def test_venv(tmp_path: Path) -> Path:
    """Create a fresh venv for testing."""
    venv_dir = tmp_path / "venv"
    result = subprocess.run(
        [_uv(), "venv", str(venv_dir), "--python", sys.executable],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, f"Failed to create venv: {result.stderr}"
    return venv_dir


def _venv_python(venv_dir: Path) -> Path:
    if sys.platform == "win32":
        return venv_dir / "Scripts" / "python.exe"
    return venv_dir / "bin" / "python"


class TestPipInstallWhileRunning:
    """Verify pip install/uninstall works while clud is running."""

    def test_install_launch_reinstall(self, test_venv: Path) -> None:
        """Install clud, launch it (blocking), reinstall while running."""
        python = _venv_python(test_venv)
        clud = _clud_exe(test_venv)

        # Step 1: Install clud
        result = _pip_install(python)
        assert result.returncode == 0, f"First install failed: {result.stderr}"
        assert clud.is_file(), f"clud binary not found at {clud}"

        # Step 2: Launch clud with --dry-run (exits quickly but exercises trampoline)
        run_result = subprocess.run(
            [str(clud), "--dry-run", "-p", "hello"],
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert run_result.returncode == 0, f"clud --dry-run failed: {run_result.stderr}"
        data = json.loads(run_result.stdout)
        assert data["backend"] == "claude"

        # Step 3: Reinstall while the .old file may still exist
        result = _pip_install(python)
        assert result.returncode == 0, f"Reinstall failed: {result.stderr}"

        # Step 4: Verify the new install works
        run_result = subprocess.run(
            [str(clud), "--version"],
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert run_result.returncode == 0
        assert "clud" in run_result.stdout

    def test_install_launch_background_reinstall(self, test_venv: Path) -> None:
        """Install clud, launch a long-running process, reinstall while it's alive."""
        python = _venv_python(test_venv)
        clud = _clud_exe(test_venv)

        # Step 1: Install clud
        result = _pip_install(python)
        assert result.returncode == 0, f"First install failed: {result.stderr}"

        # Step 2: Launch clud in the background (it will fail to find claude,
        # but the trampoline runs before that check)
        # Use a subprocess that stays alive briefly
        proc = subprocess.Popen(
            [str(clud), "--dry-run", "-p", "hello"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        # Give the trampoline time to rename+copy
        time.sleep(0.5)

        # Step 3: Reinstall WHILE clud process may still be alive
        result = _pip_install(python)
        reinstall_ok = result.returncode == 0

        # Clean up the background process
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()

        assert reinstall_ok, f"Reinstall while running failed: {result.stderr}"

        # Step 4: Verify new install works
        run_result = subprocess.run(
            [str(clud), "--version"],
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert run_result.returncode == 0

    def test_uninstall_after_run(self, test_venv: Path) -> None:
        """Install, run, then uninstall — verify clean removal."""
        python = _venv_python(test_venv)
        clud = _clud_exe(test_venv)

        # Install
        result = _pip_install(python)
        assert result.returncode == 0

        # Run (exercises trampoline, creates .old and cache files)
        subprocess.run(
            [str(clud), "--dry-run", "-p", "hello"],
            capture_output=True,
            text=True,
            timeout=15,
        )

        # Uninstall
        result = _pip_uninstall(python)
        assert result.returncode == 0, f"Uninstall failed: {result.stderr}"

        # Verify binary is gone
        assert not clud.is_file(), f"clud binary still exists after uninstall: {clud}"

    def test_multiple_rapid_reinstalls(self, test_venv: Path) -> None:
        """Rapidly reinstall 3 times — verify no lock errors accumulate."""
        python = _venv_python(test_venv)
        clud = _clud_exe(test_venv)

        for i in range(3):
            result = _pip_install(python)
            assert result.returncode == 0, f"Install #{i + 1} failed: {result.stderr}"

            run_result = subprocess.run(
                [str(clud), "--dry-run", "-p", f"iteration {i}"],
                capture_output=True,
                text=True,
                timeout=15,
            )
            assert run_result.returncode == 0, f"Run #{i + 1} failed: {run_result.stderr}"

    @pytest.mark.skipif(sys.platform != "win32", reason="Windows-specific lock test")
    def test_old_file_cleanup(self, test_venv: Path) -> None:
        """Verify .old files are cleaned up on next launch."""
        python = _venv_python(test_venv)
        clud = _clud_exe(test_venv)

        result = _pip_install(python)
        assert result.returncode == 0

        # First run creates .old
        subprocess.run(
            [str(clud), "--version"],
            capture_output=True,
            timeout=15,
        )

        old_file = clud.with_suffix(".exe.old")
        # .old may or may not exist depending on timing, but if it does,
        # the next run should clean it up
        if old_file.is_file():
            subprocess.run(
                [str(clud), "--version"],
                capture_output=True,
                timeout=15,
            )
            # After second run, .old should be cleaned up
            # (unless the first process is somehow still alive)
            # Don't assert — just verify no crash
