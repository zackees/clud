"""Playwright regression test for issue #190: `clud ui` must show live
direct-runner sessions.

Before the fix, the dashboard's `/state.json` endpoint only read
`<state_dir>/sessions/*.json` snapshot files written by the centralized
daemon worker. The default `clud` invocation runs through the *direct*
runner and never produces such a snapshot — so the dashboard rendered
"no sessions recorded." even while a `clud` was clearly running.

This test exercises the contract end-to-end:

1. Launch a long-running `clud` against the mock backend.
2. Run `clud ui --no-open` to obtain the dashboard URL.
3. Load the URL in a real Chromium tab via Playwright.
4. Wait for the Sessions card to populate and assert at least one row
   is rendered, and that the empty-state copy is gone.

The test is opt-in: it requires the `playwright` Python package and a
local Chromium install (`python -m playwright install chromium`). On
hosts where that isn't available the test self-skips so CI doesn't break
for unrelated reasons.
"""

from __future__ import annotations

import json
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

# Ensure the env-isolation helper from the registry-concurrency suite is
# importable — its `_registry_env` does the right CLUD_SESSION_DB pinning.
sys.path.insert(0, str(Path(__file__).parent))

pytestmark = pytest.mark.integration


def _playwright_or_skip():
    """Import Playwright or skip the whole test."""
    try:
        from playwright.sync_api import sync_playwright  # type: ignore[import-not-found]

        return sync_playwright
    except ImportError:
        pytest.skip(
            "playwright not installed; run `pip install playwright && "
            "python -m playwright install chromium` to enable this test."
        )


def _dashboard_env(
    base: dict[str, str], state_dir: Path, registry_dir: Path
) -> dict[str, str]:
    """Build an env that isolates the dashboard's state and registry from the host.

    The daemon writes to CLUD_DAEMON_STATE_DIR; the redb registry honors
    CLUD_SESSION_DB / CLUD_SESSION_LOCK. Pin all three so the test never
    touches the user's real `~/.clud/state` or `%LOCALAPPDATA%\\clud`.
    """
    env = dict(base)
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    env["CLUD_SESSION_DB"] = str(registry_dir / "sessions.redb")
    env["CLUD_SESSION_LOCK"] = str(registry_dir / "sessions.lock")
    env["CLUD_MAX_INSTANCES"] = "16"
    return env


def _wait_for_dashboard_port(
    state_dir: Path, timeout: float = 30.0
) -> int:
    """Poll daemon.json until it advertises a dashboard port."""
    deadline = time.time() + timeout
    info_path = state_dir / "daemon.json"
    while time.time() < deadline:
        if info_path.is_file():
            try:
                info = json.loads(info_path.read_text(encoding="utf-8"))
            except (json.JSONDecodeError, PermissionError):
                time.sleep(0.1)
                continue
            port = info.get("dashboard_port")
            if port:
                return int(port)
        time.sleep(0.2)
    raise AssertionError(
        f"daemon.json never advertised a dashboard port within {timeout}s"
    )


def _port_open(port: int, host: str = "127.0.0.1", timeout: float = 1.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _wait_for_port_open(port: int, timeout: float = 10.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if _port_open(port):
            return
        time.sleep(0.1)
    raise AssertionError(f"dashboard port {port} never opened within {timeout}s")


class TestUiDashboardShowsLiveSessions:
    """Issue #190: the dashboard must surface direct-runner sessions."""

    def test_direct_runner_session_appears_in_dashboard_via_playwright(
        self,
        clud_binary: Path,
        mock_env: dict[str, str],
        tmp_path: Path,
    ) -> None:
        sync_playwright = _playwright_or_skip()

        state_dir = tmp_path / "state"
        registry_dir = tmp_path / "registry"
        state_dir.mkdir()
        registry_dir.mkdir()
        env = _dashboard_env(mock_env, state_dir, registry_dir)

        # Copy the binary into a private dir so we don't race the global
        # clud.exe lock — see test_session_registry_concurrency for the
        # rationale.
        launch = tmp_path / clud_binary.name
        shutil.copy2(clud_binary, launch)

        # Long sleep so the session row stays in the registry while we
        # navigate the dashboard.
        clud_proc = subprocess.Popen(
            [
                str(launch),
                "-p",
                "hello",
                "--",
                "--mock-sleep-ms",
                "20000",
            ],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        try:
            port = _wait_for_dashboard_port(state_dir, timeout=30.0)
            _wait_for_port_open(port, timeout=10.0)
            url = f"http://127.0.0.1:{port}/"

            with sync_playwright() as p:
                browser = p.chromium.launch()
                try:
                    page = browser.new_page()
                    page.goto(url, wait_until="networkidle", timeout=15_000)

                    # The dashboard polls /state.json every 5s. Either it's
                    # already populated by the time the page settled, or
                    # the next poll fills it in within a few seconds. Wait
                    # for a real <tr> inside the sessions card and assert
                    # the empty-state copy is gone.
                    page.wait_for_selector(
                        "#sessions-body table tbody tr",
                        timeout=15_000,
                    )

                    body_html = page.inner_html("#sessions-body")
                    assert "no sessions recorded" not in body_html, (
                        "Dashboard rendered the empty-state copy even though "
                        f"a live clud is running. HTML:\n{body_html}"
                    )

                    # Header stats should reflect at least one live session.
                    stats = page.inner_text("#stats")
                    assert stats.strip(), "stats line was empty"
                    # Be tolerant of formatting — just look for a non-zero
                    # live count somewhere in the header line.
                    assert "0 live" not in stats, (
                        f"stats line still claims 0 live sessions: {stats!r}"
                    )
                finally:
                    browser.close()
        finally:
            try:
                clud_proc.terminate()
                clud_proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                if sys.platform == "win32":
                    subprocess.run(
                        ["taskkill", "/PID", str(clud_proc.pid), "/T", "/F"],
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                else:
                    clud_proc.kill()
                    clud_proc.wait(timeout=5)
            # Drain pipes so the zombie scanner doesn't trip on us.
            try:
                clud_proc.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                pass
