"""Integration coverage for foreground process-identity leases (#643)."""

from __future__ import annotations

import json
import sys
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


def _phase_timings_line(state_dir: Path) -> str:
    """One compact line of per-phase startup ms from the event log (#2333)."""
    parts = [
        f"{e.get('phase')}={e.get('elapsed_ms')}"
        for e in _events(state_dir)
        if e.get("op") == "daemon_startup_phase"
    ]
    return "daemon_phase_ms " + " ".join(parts)


# Generous, Windows-scaled deadlines (#2268). A hosted Windows runner has been
# seen to take longer than 10s between `daemon_starting` and the listener
# being ready, so a fixed 10s lease deadline expired with the client still
# blocked inside `ensure_daemon`. The waits are event-driven and return as soon
# as the event lands, so the generous bound costs nothing on a healthy run.
_WINDOWS = sys.platform == "win32"
DAEMON_READY_TIMEOUT = 120.0 if _WINDOWS else 30.0
LEASE_TIMEOUT = 60.0 if _WINDOWS else 15.0
PRUNE_TIMEOUT = 60.0 if _WINDOWS else 20.0
_TAIL_BYTES = 4096


def _tail_text(path: Path, limit: int = _TAIL_BYTES) -> str:
    try:
        with path.open("rb") as stream:
            stream.seek(0, 2)
            size = stream.tell()
            stream.seek(max(0, size - limit))
            return stream.read().decode("utf-8", "replace")
    except OSError as error:
        return f"<unreadable: {error}>"


def _log_tails(state_dir: Path) -> str:
    """Bounded tails of the client's captured output and any daemon log."""
    parts: list[str] = []
    candidates = [
        state_dir.parent / "client.stdout.log",
        state_dir.parent / "client.stderr.log",
    ]
    for root in (state_dir, state_dir / "logs"):
        try:
            candidates.extend(sorted(root.glob("*.log"))[:5])
        except OSError:
            continue
    for path in candidates:
        if path.is_file():
            parts.append(f"--- {path.name} (tail) ---\n{_tail_text(path)}")
    return "\n".join(parts) or "<no client or daemon logs>"


def _pid_alive(pid: int | None) -> bool | None:
    if pid is None:
        return None
    try:
        return psutil.pid_exists(pid)
    except psutil.Error:
        return None


def _wait_for_lease_event(
    state_dir: Path,
    op: str,
    client_pid: int | None,
    timeout: float = LEASE_TIMEOUT,
    *,
    proc: process.Popen[str] | None = None,
) -> dict[str, object]:
    """Wait for ``op`` (for ``client_pid``, or any client when ``None``).

    Returns as soon as the event is logged; ``timeout`` is only an upper bound.
    On failure the assertion carries bounded diagnostics: event list, whether
    the client is alive, and the client/daemon log tails.
    """
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
            if event.get("op") == op and (
                client_pid is None or event.get("client_pid") == client_pid
            ):
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
    if proc is not None and proc.poll() is not None and proc.stderr is not None:
        # Only drain a pipe once the process is gone; reading a live one blocks.
        captured = proc.stderr.read()
        stderr = (
            captured[-_TAIL_BYTES:]
            if isinstance(captured, str)
            else captured[-_TAIL_BYTES:].decode("utf-8", "replace")
        )
    status = proc.poll() if proc is not None else None
    client_alive = _pid_alive(proc.pid) if proc is not None else None
    raise AssertionError(
        f"waiting for {op} for client {client_pid} failed after {timeout}s; "
        f"exit status: {status}; client alive: {client_alive}; "
        f"event error: {event_error}; event count: {len(events)}; "
        f"all events: {str(events)[-8192:]}; stderr tail: {stderr}\n"
        f"{_log_tails(state_dir)}"
    )


def _launch_direct_client(
    clud_binary: Path,
    env: dict[str, str],
    sleep_ms: int,
    log_dir: Path | None = None,
) -> process.Popen[str]:
    # Capture to files, not pipes: a file can be tailed while the client is
    # still running, a pipe cannot without blocking.
    stdout: object = process.DEVNULL
    stderr: object = process.PIPE
    if log_dir is not None:
        log_dir.mkdir(parents=True, exist_ok=True)
        stdout = (log_dir / "client.stdout.log").open("wb")
        stderr = (log_dir / "client.stderr.log").open("wb")
    try:
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
            stdout=stdout,
            stderr=stderr,
            text=log_dir is None,
            env=env,
        )
    finally:
        if log_dir is not None:
            stdout.close()  # type: ignore[attr-defined]
            stderr.close()  # type: ignore[attr-defined]


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
    proc = _launch_direct_client(clud_binary, env, sleep_ms, state_dir.parent)
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
    capsys: pytest.CaptureFixture[str],
) -> None:
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    with _managed_client(clud_binary, env, 2_000, state_dir) as proc:
        _wait_for_lease_event(state_dir, "daemon_started", None, DAEMON_READY_TIMEOUT, proc=proc)
        with capsys.disabled():
            print(_phase_timings_line(state_dir), flush=True)
        acquired = _wait_for_lease_event(state_dir, "client_lease_acquired", proc.pid, proc=proc)
        assert acquired["lease_count"] == 1
        assert proc.wait(timeout=10) == 0
        released = _wait_for_lease_event(state_dir, "client_lease_released", proc.pid, 30.0)

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
        _wait_for_lease_event(state_dir, "daemon_started", None, DAEMON_READY_TIMEOUT, proc=proc)
        acquired = _wait_for_lease_event(state_dir, "client_lease_acquired", proc.pid, proc=proc)

        daemon_pid = acquired["daemon_pid"]
        _kill_client_tree(proc, daemon_pid)
        pruned = _wait_for_lease_event(
            state_dir,
            "client_lease_pruned",
            proc.pid,
            timeout=PRUNE_TIMEOUT,
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
