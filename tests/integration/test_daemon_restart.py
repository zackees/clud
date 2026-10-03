"""Integration coverage for `clud daemon restart` (#186) and stop (#635)."""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

import psutil
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


def _mark_daemon_newer(state_dir: Path) -> None:
    info_path = state_dir / "daemon.json"
    info = _read_daemon_info(state_dir)
    info["version"] = "999.0.0"
    info_path.write_text(json.dumps(info), encoding="utf-8")


def test_daemon_stop_and_restart_recover_from_newer_version(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)

    try:
        started = _run_restart(clud_binary, env)
        assert started.returncode == 0, started.stderr
        first_pid = int(_read_daemon_info(state_dir)["pid"])
        _mark_daemon_newer(state_dir)

        stopped = _run_stop(clud_binary, env)
        assert stopped.returncode == 0, stopped.stderr
        wait_for_pids_to_exit([first_pid], timeout=15)
        assert not (state_dir / "daemon.json").exists()

        started = _run_restart(clud_binary, env)
        assert started.returncode == 0, started.stderr
        second_pid = int(_read_daemon_info(state_dir)["pid"])
        _mark_daemon_newer(state_dir)

        restarted = _run_restart(clud_binary, env)
        assert restarted.returncode == 0, restarted.stderr
        wait_for_pids_to_exit([second_pid], timeout=15)
        replacement_pid = int(_read_daemon_info(state_dir)["pid"])
        assert replacement_pid != second_pid
        assert pid_is_alive(replacement_pid)
    finally:
        _cleanup_daemon(state_dir)


def test_daemon_stop_falls_back_when_old_daemon_rejects_shutdown(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    state_dir = tmp_path / "daemon-state"
    state_dir.mkdir()
    env = managed_env(mock_env, state_dir)
    env["CLUD_DAEMON_WIRE"] = "json"
    port_file = tmp_path / "port"
    peer = process.Popen(
        [
            sys.executable,
            "-c",
            "import socket,sys,pathlib\n"
            "listener=socket.socket()\n"
            "listener.bind(('127.0.0.1',0))\n"
            "listener.listen(1)\n"
            "pathlib.Path(sys.argv[1]).write_text(str(listener.getsockname()[1]))\n"
            "conn,_=listener.accept()\n"
            "with conn:\n"
            "    conn.recv(65536)\n"
            "    conn.sendall(b'{\"op\":\"error\",\"message\":\"old client refused\"}\\n')\n"
            "import time; time.sleep(60)\n",
            str(port_file),
        ],
        stdout=process.PIPE,
        stderr=process.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 10
        while not port_file.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        assert port_file.exists(), "fake daemon failed to start"
        assert peer.pid is not None
        (state_dir / "daemon.json").write_text(
            json.dumps(
                {
                    "pid": peer.pid,
                    "pid_start": int(psutil.Process(peer.pid).create_time()),
                    "port": int(port_file.read_text()),
                    "version": "999.0.0",
                }
            ),
            encoding="utf-8",
        )

        stopped = _run_stop(clud_binary, env)
        assert stopped.returncode == 0, stopped.stderr
        wait_for_pids_to_exit([peer.pid], timeout=15)
        assert not (state_dir / "daemon.json").exists()
    finally:
        if peer.poll() is None:
            peer.kill()
        peer.wait(timeout=10)


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
