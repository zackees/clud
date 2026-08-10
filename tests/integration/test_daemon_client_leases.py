"""Integration coverage for foreground process-identity leases (#643)."""

from __future__ import annotations

import json
import time
from pathlib import Path

import psutil
import pytest

from tests import process

from ._daemon_helpers import kill_process_only, managed_env

pytestmark = pytest.mark.integration


def _events(state_dir: Path) -> list[dict[str, object]]:
    path = state_dir / "daemon-events.jsonl"
    if not path.is_file():
        return []
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def _wait_for_lease_event(
    state_dir: Path,
    op: str,
    client_pid: int,
    timeout: float = 10.0,
) -> dict[str, object]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            events = _events(state_dir)
        except (json.JSONDecodeError, OSError):
            time.sleep(0.05)
            continue
        for event in events:
            if event.get("op") == op and event.get("client_pid") == client_pid:
                return event
        time.sleep(0.05)
    raise AssertionError(
        f"timed out waiting for {op} for client {client_pid}: {_events(state_dir)}"
    )


def _launch_direct_client(
    clud_binary: Path,
    env: dict[str, str],
    sleep_ms: int,
) -> process.Popen[str]:
    return process.Popen(
        [
            str(clud_binary),
            "--codex",
            "--subprocess",
            "-p",
            "lease-probe",
            "--",
            "--mock-sleep-ms",
            str(sleep_ms),
        ],
        stdout=process.DEVNULL,
        stderr=process.DEVNULL,
        text=True,
        env=env,
    )


def _kill_client_tree(proc: process.Popen[str], daemon_pid: int) -> None:
    """Forcibly kill the client and its backend children — but never the daemon.

    The daemon is spawned *by* the client, so on Windows it sits inside the
    client's process tree. `taskkill /T` therefore takes it down too, and this
    test then waits forever for a prune event from a daemon that no longer
    exists. That is what the tree-killing version of this helper did: the daemon
    log simply stopped after `client_lease_acquired`, with no shutdown event and
    no prune tick, which reads exactly like a product bug in the sweep.

    So: kill each process individually (`/F` without `/T`) and skip the daemon.
    Pruning the abandoned lease is precisely what we are here to observe.
    """
    try:
        descendants = [
            child.pid
            for child in psutil.Process(proc.pid).children(recursive=True)
            if child.pid != daemon_pid
        ]
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        descendants = []
    kill_process_only(proc.pid)
    try:
        proc.wait(timeout=10)
    except process.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    for pid in reversed(descendants):
        if psutil.pid_exists(pid):
            kill_process_only(pid)


def test_direct_client_holds_one_lease_and_releases_on_normal_exit(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    proc = _launch_direct_client(clud_binary, env, sleep_ms=2_000)

    acquired = _wait_for_lease_event(state_dir, "client_lease_acquired", proc.pid)
    assert acquired["lease_count"] == 1
    assert proc.wait(timeout=10) == 0
    released = _wait_for_lease_event(state_dir, "client_lease_released", proc.pid)

    assert released["lease_count"] == 0
    client_events = [
        event
        for event in _events(state_dir)
        if event.get("client_pid") == proc.pid
    ]
    assert [event["op"] for event in client_events] == [
        "client_lease_acquired",
        "client_lease_released",
    ]


def test_forcibly_killed_client_is_pruned_without_release_rpc(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    proc = _launch_direct_client(clud_binary, env, sleep_ms=30_000)
    acquired = _wait_for_lease_event(state_dir, "client_lease_acquired", proc.pid)

    daemon_pid = acquired["daemon_pid"]
    _kill_client_tree(proc, daemon_pid)
    pruned = _wait_for_lease_event(
        state_dir,
        "client_lease_pruned",
        proc.pid,
        timeout=15,
    )

    assert pruned["lease_count"] == 0
    assert not any(
        event.get("op") == "client_lease_released"
        and event.get("client_pid") == proc.pid
        for event in _events(state_dir)
    )
