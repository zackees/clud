"""Integration coverage for `clud daemon restart` (#186) and stop (#635)."""

from __future__ import annotations

import json
import time
from pathlib import Path

import pytest

from tests import process

from ._daemon_helpers import (
    kill_process,
    managed_env,
    pid_is_alive,
    wait_for_pids_to_exit,
)

pytestmark = pytest.mark.integration


def _read_daemon_info(state_dir: Path, timeout: float = 10.0) -> dict:
    info_path = state_dir / "daemon.json"
    deadline = time.time() + timeout
    while time.time() < deadline:
        if info_path.is_file():
            try:
                return json.loads(info_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                pass
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {info_path}")


def _run_restart(clud_binary: Path, env: dict[str, str]) -> process.CompletedProcess[str]:
    return process.run(
        [str(clud_binary), "daemon", "restart"],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )


def _run_stop(clud_binary: Path, env: dict[str, str]) -> process.CompletedProcess[str]:
    return process.run(
        [str(clud_binary), "daemon", "stop"],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )


def _assert_daemon_remains_stopped(state_dir: Path, duration: float = 1.0) -> None:
    info_path = state_dir / "daemon.json"
    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        if info_path.exists():
            try:
                replacement = json.loads(info_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError:
                replacement = {"raw": info_path.read_text(encoding="utf-8", errors="replace")}
            raise AssertionError(
                f"daemon respawned after stop; state_dir={state_dir}, info={replacement!r}"
            )
        time.sleep(0.05)


def _cleanup_daemon(state_dir: Path) -> None:
    info_path = state_dir / "daemon.json"
    if not info_path.is_file():
        return
    try:
        pid = int(json.loads(info_path.read_text(encoding="utf-8"))["pid"])
    except (json.JSONDecodeError, KeyError, ValueError):
        return
    if pid_is_alive(pid):
        kill_process(pid)
        wait_for_pids_to_exit([pid], timeout=15)


def test_daemon_restart_replaces_pid_and_restores_dashboard_listener(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)

    try:
        first = _run_restart(clud_binary, env)
        assert first.returncode == 0, (
            f"cold-start daemon restart failed: {first.returncode}\n"
            f"stdout: {first.stdout!r}\nstderr: {first.stderr!r}"
        )
        original_info = _read_daemon_info(state_dir)
        original_pid = int(original_info["pid"])
        assert pid_is_alive(original_pid)

        second = _run_restart(clud_binary, env)
        assert second.returncode == 0, (
            f"daemon restart failed: {second.returncode}\n"
            f"stdout: {second.stdout!r}\nstderr: {second.stderr!r}"
        )

        wait_for_pids_to_exit([original_pid], timeout=15)
        new_info = _read_daemon_info(state_dir)
        new_pid = int(new_info["pid"])

        assert new_pid != original_pid
        assert pid_is_alive(new_pid)
        assert new_info.get("dashboard_port"), (
            f"replacement daemon must have a dashboard listener; got {new_info!r}"
        )
    finally:
        _cleanup_daemon(state_dir)


def test_daemon_stop_is_idempotent_and_does_not_respawn(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)

    try:
        started = _run_restart(clud_binary, env)
        assert started.returncode == 0, started.stderr
        info = _read_daemon_info(state_dir)
        daemon_pid = int(info["pid"])
        assert pid_is_alive(daemon_pid)

        stopped = _run_stop(clud_binary, env)
        assert stopped.returncode == 0, stopped.stderr
        wait_for_pids_to_exit([daemon_pid], timeout=15)
        assert not (state_dir / "daemon.json").exists()
        _assert_daemon_remains_stopped(state_dir)

        stopped_again = _run_stop(clud_binary, env)
        assert stopped_again.returncode == 0, stopped_again.stderr
        assert not (state_dir / "daemon.json").exists()
    finally:
        _cleanup_daemon(state_dir)
