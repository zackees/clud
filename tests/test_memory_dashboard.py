"""Issue #263: surface-level checks for the Memory dashboard tab.

These tests run the embedded dashboard's index.html and the `/memory/*`
HTTP routes through a real `clud` binary and assert the contract the
vanilla-JS SPA relies on. We deliberately do not exercise the JS in a
headless browser here — that's owned by the Playwright suite in
`tests/integration/test_ui_dashboard_playwright.py`. This module is
fast: it boots the daemon, hits two HTTP endpoints, and tears down.

If `clud` is missing or the daemon can't start (Windows-ARM with no
embedder, sandbox without listener perms, etc.) the test self-skips
rather than failing loudly.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent


def _cargo_argv_and_env(subcommand: list[str]) -> tuple[list[str], dict[str, str]]:
    if sys.platform == "win32":
        try:
            from ci.env import build_env, cargo_argv

            env = build_env()
            return cargo_argv(subcommand, env=env), env
        except Exception:
            pass
        soldr = shutil.which("soldr")
        if soldr:
            return [soldr, "cargo", *subcommand], dict(os.environ)
    return ["cargo", *subcommand], dict(os.environ)


def _build_clud() -> Path:
    env_binary = os.environ.get("CLUD_TEST_BINARY")
    if env_binary and Path(env_binary).is_file():
        return Path(env_binary)
    argv, build_env_vars = _cargo_argv_and_env(
        ["build", "-p", "clud", "--no-default-features", "--message-format=json"]
    )
    result = subprocess.run(
        argv,
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=600,
        env=build_env_vars,
    )
    if result.returncode != 0:
        pytest.skip(f"could not build clud:\n{result.stderr[-2000:]}")
    for line in result.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "clud"
            and msg.get("executable")
        ):
            return Path(msg["executable"])
    pytest.skip("could not locate clud binary in cargo JSON output")


def _wait_for_port(state_dir: Path, timeout: float = 30.0) -> int:
    """Poll daemon.json for the dashboard port."""
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
                # Also wait for the listener to be reachable.
                deadline2 = time.time() + 10.0
                while time.time() < deadline2:
                    try:
                        with socket.create_connection(
                            ("127.0.0.1", int(port)), timeout=1.0
                        ):
                            return int(port)
                    except OSError:
                        time.sleep(0.1)
                return int(port)
        time.sleep(0.2)
    raise AssertionError(
        f"daemon.json never advertised a dashboard port within {timeout}s"
    )


def _fetch(url: str, timeout: float = 5.0) -> tuple[int, bytes]:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as resp:
            return resp.getcode(), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


class _DaemonHandle:
    def __init__(self, clud: Path, state_dir: Path) -> None:
        self.clud = clud
        self.state_dir = state_dir
        env = os.environ.copy()
        env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
        # Disable the embedder so this test runs on every platform —
        # otherwise fastembed would try to download a model and stall
        # CI on first run.
        env["CLUD_MEMORY_EMBEDDER"] = "disabled"
        try:
            self.proc = subprocess.Popen(
                [str(clud), "__daemon", "--state-dir", str(state_dir)],
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except (FileNotFoundError, PermissionError) as e:
            pytest.skip(f"could not spawn clud daemon: {e}")

    def close(self) -> None:
        try:
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except (subprocess.TimeoutExpired, ProcessLookupError):
            if sys.platform == "win32":
                subprocess.run(
                    ["taskkill", "/PID", str(self.proc.pid), "/T", "/F"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
            else:
                self.proc.kill()
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass


@pytest.fixture(scope="module")
def clud_binary() -> Path:
    return _build_clud()


@pytest.fixture
def daemon(clud_binary: Path):
    with tempfile.TemporaryDirectory(prefix="clud-memdash-") as tmp:
        state_dir = Path(tmp) / "state"
        state_dir.mkdir()
        handle = _DaemonHandle(clud_binary, state_dir)
        try:
            port = _wait_for_port(state_dir, timeout=30.0)
            yield port
        finally:
            handle.close()


def test_dashboard_serves_memory_tab_markup(daemon: int) -> None:
    """The bundled SPA must include the Memory tab — both the tab
    button and the section — for the dashboard to render the 5th tab
    visualization described in issue #263."""
    status, body = _fetch(f"http://127.0.0.1:{daemon}/")
    assert status == 200, body[:400]
    html = body.decode("utf-8", errors="replace")
    assert 'id="tab-memory"' in html, "tab button missing"
    assert 'data-tab="memory"' in html, "tab data attribute missing"
    assert 'id="memory"' in html, "section missing"
    # The render code expects these endpoint references to live in the
    # client. Drop them in a refactor and this test catches it.
    assert "/memory/stats" in html
    assert "/memory/recent" in html
    assert "/memory/search" in html
    # Hash routing — `#memory` deep link from CLI `clud memory ui` must
    # land on the Memory tab.
    assert "applyHashRoute" in html


def test_memory_stats_endpoint_returns_dashboard_shape(daemon: int) -> None:
    """The dashboard's `renderMemoryStats` reads these fields. If the
    daemon ever drops one, the tile silently renders 'undefined' — so
    pin the contract here."""
    status, body = _fetch(f"http://127.0.0.1:{daemon}/memory/stats")
    # 503 is acceptable on hosts where the memory service couldn't
    # start (e.g. Windows-ARM without ort 2.0). Otherwise must be 200.
    if status == 503:
        pytest.skip("memory subsystem unavailable on this host")
    assert status == 200, body[:400]
    payload = json.loads(body)
    assert "tier_counts" in payload, payload
    tc = payload["tier_counts"]
    for tier in ("working", "episodic", "semantic"):
        assert tier in tc, f"missing tier_counts.{tier} in {payload}"
        assert isinstance(tc[tier], int), tc
    # Embedder fields — at minimum one of these must exist for the
    # embedder pill to render. The route exposes all three today.
    assert "embedder_status" in payload, payload
    assert "embedder_dim" in payload, payload
    assert "store_embed_dim" in payload, payload
    assert "schema_user_version" in payload, payload
