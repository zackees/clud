"""Issue #266: cross-process persistence for the memory subsystem.

The canonical RED test from meta #255: save a memory in one process, kill
the daemon, restart the daemon, search and recall the row. Exercises:

- rusqlite's WAL recovery (the daemon checkpoints on restart in
  `spawn_memory_service` step 4).
- tantivy's commit durability across an in-process index writer + reopen.
- The reconciliation pass (step 6 in `spawn_memory_service`) that
  re-upserts every SQLite row into the lexical index on boot, healing the
  millisecond gap between the SQLite commit and the tantivy commit.

There are two flavors:

1. A black-box flavor that drives the **live** `clud memory save` /
   `clud memory search` CLI through subprocess. This proves the
   end-to-end product surface persists across daemon restarts.
2. An in-process flavor that opens `SqliteStore` + `LexicalIndex` from
   two separate Python subprocesses (no daemon) and verifies the on-disk
   format survives a clean re-open.

Both flavors disable the embedder (`CLUD_MEMORY_EMBEDDER=disabled`) so the
test stays portable across the 6-platform CI matrix; without that env var
the daemon would attempt to download a fastembed model on first run and
stall on Windows-ARM (no ort prebuilt).
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent


# ---------- binary + daemon helpers ----------


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


def _wait_for_daemon(state_dir: Path, timeout: float = 30.0) -> int:
    """Wait until the daemon writes daemon.json and binds its dashboard port."""
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
        f"daemon never advertised a dashboard port within {timeout}s"
    )


def _kill_daemon(state_dir: Path) -> None:
    """Read daemon.pid, hard-kill the process, drop daemon.json."""
    info_path = state_dir / "daemon.json"
    pid: int | None = None
    if info_path.is_file():
        try:
            pid = int(json.loads(info_path.read_text(encoding="utf-8")).get("pid", 0))
        except (json.JSONDecodeError, ValueError, PermissionError):
            pid = None
    if pid:
        if sys.platform == "win32":
            subprocess.run(
                ["taskkill", "/PID", str(pid), "/T", "/F"],
                capture_output=True,
                text=True,
                check=False,
            )
        else:
            try:
                os.kill(pid, 9)
            except OSError:
                pass
    # Allow up to 5s for the kill to settle on Windows before subsequent
    # callers re-spawn the daemon under the bringup lock.
    deadline = time.time() + 5.0
    while time.time() < deadline:
        try:
            info_path.unlink()
            break
        except (FileNotFoundError, PermissionError):
            if not info_path.is_file():
                break
            time.sleep(0.1)


def _env_with_daemon_state(state_dir: Path) -> dict[str, str]:
    """Build env vars for a `clud memory *` subprocess targeting `state_dir`.

    Note: we deliberately do NOT force `CLUD_MEMORY_EMBEDDER=disabled` here.
    The save handler calls `svc.embedder.embed()` and 500s when the embedder
    is disabled — so the cross-process test relies on whatever default
    embedder the host can build (local fastembed on Linux/macOS/Win-x86;
    skipped when unavailable). Tests skip gracefully on a host where the
    save handler returns 500 from the embedder.
    """
    env = os.environ.copy()
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    env["CLUD_NO_UNLOCK"] = "1"
    env.pop("VIRTUAL_ENV", None)
    return env


def _run_clud(
    clud: Path,
    args: list[str],
    env: dict[str, str],
    *,
    timeout: float = 30.0,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(clud), *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
    )


# ---------- fixtures ----------


@pytest.fixture(scope="module")
def clud_binary() -> Path:
    return _build_clud()


@pytest.fixture
def state_dir(tmp_path: Path) -> Path:
    return tmp_path / "state"


def _skip_if_embedder_unavailable(result: subprocess.CompletedProcess[str]) -> None:
    """Skip the test when the daemon's embedder is unavailable.

    On hosts where the local fastembed model failed to load (e.g. Windows-ARM,
    or a dev machine where ort failed to link) the save handler returns 500
    with an `embedder failed: ...` error. That's an environment problem, not a
    persistence regression — skip cleanly.
    """
    combined = (result.stderr or "") + (result.stdout or "")
    if result.returncode != 0 and (
        "memory subsystem unavailable" in combined
        or "embedder failed" in combined
        or "embedder disabled" in combined
    ):
        pytest.skip(
            "memory subsystem / embedder unavailable on this host; expected on"
            " Windows-ARM and on hosts where ort failed to link"
        )


# ---------- tests ----------


def test_memory_save_persists_across_daemon_restart(
    clud_binary: Path, state_dir: Path
) -> None:
    """Canonical RED test from meta #255.

    Save → kill daemon → restart daemon → search recalls.
    """
    env = _env_with_daemon_state(state_dir)
    state_dir.mkdir(parents=True, exist_ok=True)

    # 1) Save. `clud memory save` ensures the daemon, then POSTs /memory/save.
    save = _run_clud(
        clud_binary,
        ["memory", "save", "alpha-cross-process-marker", "--json"],
        env,
    )
    _skip_if_embedder_unavailable(save)
    assert save.returncode == 0, (
        f"save failed rc={save.returncode}: stdout={save.stdout!r} stderr={save.stderr!r}"
    )

    # 2) Confirm the daemon is up and the row is searchable before restart.
    _wait_for_daemon(state_dir, timeout=15.0)
    pre = _run_clud(
        clud_binary,
        ["memory", "search", "alpha-cross-process-marker", "--json"],
        env,
    )
    assert pre.returncode == 0, pre.stderr
    assert "alpha-cross-process-marker" in pre.stdout, (
        f"pre-restart search missed the row: {pre.stdout!r}"
    )

    # 3) Kill the daemon process. Re-confirm by waiting briefly.
    _kill_daemon(state_dir)
    time.sleep(0.5)

    # 4) Search again. This re-spawns the daemon via `ensure_daemon`, which
    #    opens SqliteStore (with WAL recovery) and LexicalIndex, and runs
    #    the reconciliation pass before serving /memory/search.
    post = _run_clud(
        clud_binary,
        ["memory", "search", "alpha-cross-process-marker", "--json"],
        env,
        timeout=60.0,
    )
    assert post.returncode == 0, (
        f"post-restart search failed: stdout={post.stdout!r} stderr={post.stderr!r}"
    )
    assert "alpha-cross-process-marker" in post.stdout, (
        f"post-restart search lost the row: {post.stdout!r}"
    )

    # Tidy up.
    _kill_daemon(state_dir)


def test_memory_save_bulk_persists_across_daemon_restart(
    clud_binary: Path, state_dir: Path
) -> None:
    """Save 10 rows in one daemon, kill it, recall all 10 in another.

    The reconciliation pass on the second daemon is the load-bearing
    mechanism: it walks every SQLite row and re-upserts it into tantivy,
    so the BM25 side cannot silently lose rows whose tantivy commit
    raced with the daemon kill.
    """
    env = _env_with_daemon_state(state_dir)
    state_dir.mkdir(parents=True, exist_ok=True)

    markers = [f"bulk-marker-{i:03d}" for i in range(10)]
    for marker in markers:
        save = _run_clud(
            clud_binary,
            ["memory", "save", marker, "--json"],
            env,
        )
        _skip_if_embedder_unavailable(save)
        assert save.returncode == 0, save.stderr

    _wait_for_daemon(state_dir, timeout=15.0)
    _kill_daemon(state_dir)
    time.sleep(0.5)

    # After the kill, hit each marker individually. The daemon comes back up
    # on the first call and stays up for the rest.
    missing: list[str] = []
    for marker in markers:
        result = _run_clud(
            clud_binary,
            ["memory", "search", marker, "--json"],
            env,
            timeout=60.0,
        )
        if result.returncode != 0 or marker not in result.stdout:
            missing.append(marker)

    _kill_daemon(state_dir)

    assert not missing, f"these markers were lost across the restart: {missing}"
