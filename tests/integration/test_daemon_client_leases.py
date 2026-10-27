"""Integration coverage for foreground process-identity leases (#643)."""

from __future__ import annotations

import json
import time
from contextlib import contextmanager
from pathlib import Path

import psutil
import pytest

from tests import process

from ._daemon_helpers import kill_process_only, managed_env

pytestmark = pytest.mark.integration


class EventLogTooLargeError(ValueError):
    pass


def _events(state_dir: Path) -> list[dict[str, object]]:
    path = state_dir / "daemon-events.jsonl"
    if not path.is_file():
        return []
    with path.open("rb") as stream:
        data = stream.read(65537)
    if len(data) > 65536:
        raise EventLogTooLargeError("daemon event log exceeds 65536 bytes")
    events = [json.loads(line) for line in data.decode("utf-8").splitlines() if line.strip()]
    if any(not isinstance(event, dict) for event in events):
        raise ValueError("daemon event row is not an object")
    return events


def _wait_for_lease_event(
    state_dir: Path,
    op: str,
    client_pid: int,
    timeout: float = 10.0,
    *,
    proc: process.Popen[str] | None = None,
) -> dict[str, object]:
    deadline = time.monotonic() + timeout
    event_error = None
    while time.monotonic() < deadline:
        try:
            events = _events(state_dir)
        except EventLogTooLargeError as error:
            event_error = str(error)
            break
        except (ValueError, UnicodeError, OSError):
            event_error = "event log unreadable or incomplete"
            events = []
        for event in events:
            if event.get("op") == op and event.get("client_pid") == client_pid:
                return event
        if proc is not None and proc.poll() is not None:
            break
        time.sleep(0.05)
    try:
        events = _events(state_dir)
    except EventLogTooLargeError as error:
        events = []
        event_error = str(error)
    except (ValueError, UnicodeError, OSError):
        events = []
        event_error = "event log unreadable or incomplete"
    stderr = ""
    if proc is not None and proc.stderr is not None:
        captured = proc.stderr.read()
        stderr = (
            captured[-4096:]
            if isinstance(captured, str)
            else captured[-4096:].decode("utf-8", "replace")
        )
    status = proc.poll() if proc is not None else None
    raise AssertionError(
        f"waiting for {op} for client {client_pid} failed; "
        f"exit status: {status}; event error: {event_error}; event count: {len(events)}; "
        f"recent events: {str(events[-10:])[-2048:]}; stderr tail: {stderr}"
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
        stderr=process.PIPE,
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


@contextmanager
def _managed_client(clud_binary: Path, env: dict[str, str], sleep_ms: int, state_dir: Path):
    proc = _launch_direct_client(clud_binary, env, sleep_ms)
    try:
        yield proc
    finally:
        if proc.poll() is None:
            # Without an acquired lease there is no verified daemon identity.
            # Kill only the client; the isolated daemon fixture owns its cleanup.
            kill_process_only(proc.pid)
            proc.wait(timeout=10)


def test_direct_client_holds_one_lease_and_releases_on_normal_exit(
    clud_binary: Path,
    mock_env: dict[str, str],
    tmp_path: Path,
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    with _managed_client(clud_binary, env, 2_000, state_dir) as proc:
        acquired = _wait_for_lease_event(state_dir, "client_lease_acquired", proc.pid, proc=proc)
        assert acquired["lease_count"] == 1
        assert proc.wait(timeout=10) == 0
        released = _wait_for_lease_event(state_dir, "client_lease_released", proc.pid)

        assert released["lease_count"] == 0
        client_events = [
            event for event in _events(state_dir) if event.get("client_pid") == proc.pid
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
    with _managed_client(clud_binary, env, 30_000, state_dir) as proc:
        acquired = _wait_for_lease_event(state_dir, "client_lease_acquired", proc.pid, proc=proc)

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
            event.get("op") == "client_lease_released" and event.get("client_pid") == proc.pid
            for event in _events(state_dir)
        )


def test_lease_wait_reports_early_client_failure(tmp_path: Path) -> None:
    import sys

    proc = process.Popen(
        [
            sys.executable,
            "-c",
            "import sys; print('fixture startup error', file=sys.stderr); sys.exit(7)",
        ],
        stdout=process.DEVNULL,
        stderr=process.PIPE,
        text=True,
    )
    assert proc.wait(timeout=10) == 7
    with pytest.raises(AssertionError) as failure:
        _wait_for_lease_event(tmp_path, "client_lease_acquired", proc.pid, timeout=0.01, proc=proc)
    assert "fixture startup error" in str(failure.value)
    assert "7" in str(failure.value)


def test_managed_client_stops_client_when_acquisition_fails(tmp_path: Path, monkeypatch) -> None:
    import sys

    proc = process.Popen(
        [sys.executable, "-c", "import time; time.sleep(30)"],
        stdout=process.DEVNULL,
        stderr=process.PIPE,
        text=True,
    )
    monkeypatch.setattr(
        __import__(__name__, fromlist=["_launch_direct_client"]),
        "_launch_direct_client",
        lambda *_: proc,
    )
    (tmp_path / "daemon-events.jsonl").write_text('{"op":"daemon_starting","daemon_pid":0}\n')
    monkeypatch.setattr(
        __import__(__name__, fromlist=["_kill_client_tree"]),
        "_kill_client_tree",
        lambda *_: pytest.fail("must not guess a daemon identity"),
    )
    with pytest.raises(RuntimeError):
        with _managed_client(Path(sys.executable), {}, 30_000, tmp_path):
            raise RuntimeError("acquisition failed")
    assert proc.poll() is not None


def test_event_log_overflow_is_reported_without_unbounded_read(tmp_path: Path) -> None:
    (tmp_path / "daemon-events.jsonl").write_bytes(b"x" * 65537)
    with pytest.raises(ValueError, match="exceeds 65536 bytes"):
        _events(tmp_path)


def test_lease_wait_reports_event_overflow_with_client_status(tmp_path: Path) -> None:
    import sys

    (tmp_path / "daemon-events.jsonl").write_bytes(b"x" * 65537)
    proc = process.Popen(
        [
            sys.executable,
            "-c",
            "import sys; print('overflow fixture', file=sys.stderr); sys.exit(7)",
        ],
        stdout=process.DEVNULL,
        stderr=process.PIPE,
        text=True,
    )
    assert proc.wait(timeout=10) == 7
    with pytest.raises(AssertionError) as failure:
        _wait_for_lease_event(tmp_path, "client_lease_acquired", proc.pid, proc=proc)
    assert "exceeds 65536 bytes" in str(failure.value)
    assert "overflow fixture" in str(failure.value)
    assert "exit status: 7" in str(failure.value)


@pytest.mark.parametrize("partial", [b'{"op":', b"\xff", b"null\n", b"[]\n"])
def test_lease_wait_handles_partial_event_log_and_client_exit(
    tmp_path: Path, partial: bytes
) -> None:
    import sys

    (tmp_path / "daemon-events.jsonl").write_bytes(partial)
    proc = process.Popen(
        [sys.executable, "-c", "import sys; sys.exit(7)"],
        stdout=process.DEVNULL,
        stderr=process.PIPE,
        text=True,
    )
    assert proc.wait(timeout=10) == 7
    started = time.monotonic()
    with pytest.raises(AssertionError) as failure:
        _wait_for_lease_event(tmp_path, "client_lease_acquired", proc.pid, timeout=5, proc=proc)
    assert time.monotonic() - started < 2
    assert "unreadable or incomplete" in str(failure.value)
    assert "exit status: 7" in str(failure.value)
