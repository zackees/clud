"""Integration coverage for the self-expiring test daemon profile (#642)."""

from __future__ import annotations

import json
import subprocess
import time
import urllib.request
from pathlib import Path

import pytest

from ._daemon_helpers import (
    daemon_env,
    launch_daemonized,
    launch_detached,
    managed_env,
    process_identity_is_alive,
    session_metadata,
    wait_for_exit,
)

pytestmark = pytest.mark.integration


def _start_daemon(
    clud_binary: Path,
    env: dict[str, str],
    state_dir: Path,
) -> dict[str, object]:
    result = subprocess.run(
        [str(clud_binary), "daemon", "restart"],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert result.returncode == 0, result.stderr
    return json.loads((state_dir / "daemon.json").read_text(encoding="utf-8"))


def _wait_for_identity_exit(info: dict[str, object], timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not process_identity_is_alive(
            int(info["pid"]),
            int(info["pid_start"]),
        ):
            return
        time.sleep(0.05)
    raise AssertionError(f"daemon identity survived test hard maximum: {info}")


def _wait_for_owned_identities(
    state_dir: Path,
    session_id: str,
    timeout: float = 10.0,
) -> list[tuple[int, int]]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        metadata = session_metadata(state_dir, session_id)
        root_pid = metadata.get("root_pid")
        root_start = int(metadata.get("root_pid_start", 0))
        worker_pid = metadata.get("worker_pid")
        worker_start = int(metadata.get("worker_pid_start", 0))
        if (
            root_pid is not None
            and root_start > 0
            and worker_pid is not None
            and worker_start > 0
        ):
            return [
                (int(worker_pid), worker_start),
                (int(root_pid), root_start),
            ]
        time.sleep(0.05)
    raise AssertionError(
        f"worker/backend identities were not recorded for session {session_id}"
    )


def _wait_for_identities_exit(
    identities: list[tuple[int, int]],
    timeout: float = 10.0,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not any(
            process_identity_is_alive(pid, start_time)
            for pid, start_time in identities
        ):
            return
        time.sleep(0.05)
    raise AssertionError(f"owned process identities survived daemon exit: {identities}")


def _events(state_dir: Path) -> list[dict[str, object]]:
    path = state_dir / "daemon-events.jsonl"
    if not path.is_file():
        return []
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def _wait_for_events(
    state_dir: Path,
    expected_ops: set[str],
    timeout: float = 10.0,
) -> set[str]:
    deadline = time.monotonic() + timeout
    ops: set[str] = set()
    while time.monotonic() < deadline:
        try:
            ops = {str(event["op"]) for event in _events(state_dir)}
        except (json.JSONDecodeError, OSError):
            time.sleep(0.05)
            continue
        if expected_ops <= ops:
            return ops
        time.sleep(0.05)
    raise AssertionError(
        f"daemon events did not include {sorted(expected_ops)}: {sorted(ops)}"
    )


def _dashboard_state_request(info: dict[str, object]) -> urllib.request.Request:
    """Build an authenticated request for the daemon's capability dashboard."""
    dashboard_port = info.get("dashboard_port")
    dashboard_token = info.get("dashboard_token")
    assert isinstance(dashboard_port, int), info
    assert isinstance(dashboard_token, str), info
    return urllib.request.Request(
        f"http://127.0.0.1:{dashboard_port}/state.json",
        headers={"Cookie": f"clud_dashboard_token={dashboard_token}"},
    )


def _production_idle_env(
    mock_env: dict[str, str], state_dir: Path, home: Path, timeout_secs: int
) -> dict[str, str]:
    """Isolated production-mode daemon environment with an explicit timeout."""
    env = mock_env.copy()
    for name in (
        "CLUD_DAEMON_TEST_MODE",
        "CLUD_DAEMON_TEST_MAX_LIFETIME_SECS",
        "CLUD_DAEMON_TEST_IDLE_TIMEOUT_SECS",
        "CLUD_DAEMON_TEST_HOST_SCANS",
    ):
        env.pop(name, None)
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    env["CLUD_EXPERIMENTAL_DAEMON"] = "1"
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    clud_home = home / ".clud"
    clud_home.mkdir(parents=True)
    (clud_home / "settings.json").write_text(
        json.dumps({"daemon": {"idle_timeout_secs": timeout_secs}}),
        encoding="utf-8",
    )
    return env


def _production_default_idle_env(
    mock_env: dict[str, str], state_dir: Path, home: Path
) -> dict[str, str]:
    """Production profile using the seeded 900-second daemon setting."""
    env = mock_env.copy()
    for name in (
        "CLUD_DAEMON_TEST_MODE",
        "CLUD_DAEMON_TEST_MAX_LIFETIME_SECS",
        "CLUD_DAEMON_TEST_IDLE_TIMEOUT_SECS",
        "CLUD_DAEMON_TEST_HOST_SCANS",
    ):
        env.pop(name, None)
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    env["CLUD_EXPERIMENTAL_DAEMON"] = "1"
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    return env


def test_test_daemon_exits_at_hard_max_without_another_request(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = daemon_env(mock_env, state_dir)
    env["CLUD_DAEMON_TEST_MAX_LIFETIME_SECS"] = "4"
    env["CLUD_DAEMON_TEST_IDLE_TIMEOUT_SECS"] = "30"
    proc, session_id = launch_daemonized(
        clud_binary,
        env,
        "--codex",
        "-p",
        "hard-expiry",
        "--",
        "--mock-sleep-ms",
        "30000",
    )
    proc.kill()
    proc.wait(timeout=10)
    owned_identities = _wait_for_owned_identities(state_dir, session_id)
    info = json.loads((state_dir / "daemon.json").read_text(encoding="utf-8"))

    _wait_for_identity_exit(info, timeout=12)
    _wait_for_identities_exit(owned_identities)

    assert not (state_dir / "daemon.json").exists()
    assert "daemon_test_max_lifetime_expired" in {
        event["op"] for event in _events(state_dir)
    }


def test_test_daemon_exits_after_safe_idle_window(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    env["CLUD_DAEMON_TEST_MAX_LIFETIME_SECS"] = "20"
    env["CLUD_DAEMON_TEST_IDLE_TIMEOUT_SECS"] = "2"
    info = _start_daemon(clud_binary, env, state_dir)

    _wait_for_identity_exit(info)

    assert "daemon_test_idle_expired" in {
        event["op"] for event in _events(state_dir)
    }


def test_configured_production_idle_timeout_exits_and_next_client_restarts(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = _production_idle_env(mock_env, state_dir, tmp_path / "home", 2)
    first = _start_daemon(clud_binary, env, state_dir)

    _wait_for_identity_exit(first, timeout=8)
    assert not (state_dir / "daemon.json").exists()
    shutdown = next(event for event in _events(state_dir) if event["op"] == "daemon_idle_shutdown")
    assert {
        "timeout_secs",
        "idle_ms",
        "worker_count",
        "lease_count",
        "active_connections",
        "active_jobs",
    } <= shutdown.keys()

    second = _start_daemon(clud_binary, env, state_dir)
    assert (int(second["pid"]), int(second["pid_start"])) != (
        int(first["pid"]),
        int(first["pid_start"]),
    )
    stop = subprocess.run(
        [str(clud_binary), "daemon", "stop"],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert stop.returncode == 0, stop.stderr


def test_production_idle_timeout_defaults_to_fifteen_minutes(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    home = tmp_path / "home"
    env = _production_default_idle_env(mock_env, state_dir, home)
    _start_daemon(clud_binary, env, state_dir)

    profile = next(
        event for event in _events(state_dir) if event["op"] == "daemon_runtime_profile"
    )
    assert profile["production_idle_timeout_secs"] == 900
    settings = json.loads((home / ".clud" / "settings.json").read_text(encoding="utf-8"))
    assert settings["daemon"]["idle_timeout_secs"] == 900

    stop = subprocess.run(
        [str(clud_binary), "daemon", "stop"],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert stop.returncode == 0, stop.stderr


def test_dashboard_polling_blocks_configured_production_idle_timeout(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = _production_idle_env(mock_env, state_dir, tmp_path / "home", 2)
    info = _start_daemon(clud_binary, env, state_dir)

    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        with urllib.request.urlopen(_dashboard_state_request(info), timeout=2) as response:
            assert response.status == 200
            response.read()
        assert process_identity_is_alive(int(info["pid"]), int(info["pid_start"]))
        time.sleep(0.2)

    _wait_for_identity_exit(info, timeout=6)
    assert "daemon_idle_shutdown" in {event["op"] for event in _events(state_dir)}


def test_detached_worker_blocks_configured_production_idle_timeout(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = _production_idle_env(mock_env, state_dir, tmp_path / "home", 2)
    proc, _session_id = launch_detached(
        clud_binary,
        env,
        "--codex",
        "-p",
        "worker-keeps-daemon-alive",
        "--",
        "--mock-sleep-ms",
        "4000",
    )
    assert wait_for_exit(proc, timeout=10) == 0
    info = json.loads((state_dir / "daemon.json").read_text(encoding="utf-8"))

    time.sleep(3)
    assert process_identity_is_alive(int(info["pid"]), int(info["pid_start"]))
    _wait_for_identity_exit(info, timeout=8)
    assert "daemon_idle_shutdown" in {event["op"] for event in _events(state_dir)}


def test_foreground_client_lease_blocks_configured_production_idle_timeout(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = _production_idle_env(mock_env, state_dir, tmp_path / "home", 2)
    client = subprocess.Popen(
        [
            str(clud_binary),
            "--codex",
            "--subprocess",
            "-p",
            "foreground-lease-keeps-daemon-alive",
            "--",
            "--mock-sleep-ms",
            "5000",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=True,
        env=env,
    )
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline and not (state_dir / "daemon.json").is_file():
        time.sleep(0.05)
    assert (state_dir / "daemon.json").is_file(), "foreground client never started a daemon"
    info = json.loads((state_dir / "daemon.json").read_text(encoding="utf-8"))

    # No repeated daemon RPC is sent here. The client lease alone must outlive
    # the two-second idle setting while the foreground process is active.
    time.sleep(3)
    assert process_identity_is_alive(int(info["pid"]), int(info["pid_start"]))
    assert wait_for_exit(client, timeout=10) == 0
    _wait_for_identity_exit(info, timeout=8)
    assert "daemon_idle_shutdown" in {event["op"] for event in _events(state_dir)}


def test_zero_production_idle_timeout_remains_disabled(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = _production_idle_env(mock_env, state_dir, tmp_path / "home", 0)
    info = _start_daemon(clud_binary, env, state_dir)

    time.sleep(3)
    assert process_identity_is_alive(int(info["pid"]), int(info["pid_start"]))
    stop = subprocess.run(
        [str(clud_binary), "daemon", "stop"],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )
    assert stop.returncode == 0, stop.stderr


def test_dashboard_activity_defers_test_idle_expiry(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    env["CLUD_DAEMON_TEST_MAX_LIFETIME_SECS"] = "20"
    env["CLUD_DAEMON_TEST_IDLE_TIMEOUT_SECS"] = "2"
    info = _start_daemon(clud_binary, env, state_dir)

    polling_deadline = time.monotonic() + 4
    while time.monotonic() < polling_deadline:
        with urllib.request.urlopen(_dashboard_state_request(info), timeout=2) as response:
            assert response.status == 200
            response.read()
        assert process_identity_is_alive(
            int(info["pid"]),
            int(info["pid_start"]),
        )
        time.sleep(0.25)

    _wait_for_identity_exit(info, timeout=6)
    assert "daemon_test_idle_expired" in {
        event["op"] for event in _events(state_dir)
    }


def test_test_session_starts_no_host_scanners(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    proc, _session_id = launch_detached(
        clud_binary,
        env,
        "--codex",
        "-p",
        "scanner-free",
        "--",
        "--mock-sleep-ms",
        "300",
    )
    assert wait_for_exit(proc, timeout=10) == 0
    events = _events(state_dir)
    profile = next(event for event in events if event["op"] == "daemon_runtime_profile")
    ops = {event["op"] for event in events}

    assert profile["test_mode"] is True
    assert profile["host_scans_enabled"] is False
    assert profile["periodic_maintenance_enabled"] is False
    assert "proc_sampler_started" not in ops
    assert "orphan_sweeper_started" not in ops


def test_test_host_scan_opt_in_starts_scanners(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    env["CLUD_DAEMON_TEST_HOST_SCANS"] = "1"
    _start_daemon(clud_binary, env, state_dir)
    ops = _wait_for_events(
        state_dir,
        {"proc_sampler_started", "orphan_sweeper_started"},
    )

    assert "proc_sampler_started" in ops
    assert "orphan_sweeper_started" in ops
