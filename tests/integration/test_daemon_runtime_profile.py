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
    dashboard_port = int(info["dashboard_port"])

    polling_deadline = time.monotonic() + 4
    while time.monotonic() < polling_deadline:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{dashboard_port}/state.json",
            timeout=2,
        ) as response:
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
    ops = {event["op"] for event in _events(state_dir)}

    assert "proc_sampler_started" in ops
    assert "orphan_sweeper_started" in ops
