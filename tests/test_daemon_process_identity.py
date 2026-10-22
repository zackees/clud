"""Daemon teardown must distinguish live identities from unreaped zombies."""

from __future__ import annotations

import psutil
import pytest

from tests.integration import _daemon_helpers


class RecordedProcess:
    def __init__(self, status: str):
        self.recorded_status = status

    def is_running(self) -> bool:
        return True

    def create_time(self) -> float:
        return 1_700_000_000.5

    def status(self) -> str:
        return self.recorded_status


@pytest.mark.parametrize("status", [psutil.STATUS_ZOMBIE, psutil.STATUS_DEAD])
def test_zombie_is_not_a_live_daemon_identity(monkeypatch, status):
    process = RecordedProcess(status)
    monkeypatch.setattr(psutil, "Process", lambda _pid: process)
    assert not _daemon_helpers.process_identity_is_alive(123, 1_700_000_000)


@pytest.mark.parametrize(
    "status",
    [
        psutil.STATUS_RUNNING,
        psutil.STATUS_SLEEPING,
        psutil.STATUS_STOPPED,
        psutil.STATUS_DISK_SLEEP,
    ],
)
def test_live_or_stopped_daemon_identity_is_preserved(monkeypatch, status):
    process = RecordedProcess(status)
    monkeypatch.setattr(psutil, "Process", lambda _pid: process)
    assert _daemon_helpers.process_identity_is_alive(123, 1_700_000_000)
    assert not _daemon_helpers.process_identity_is_alive(123, 1_700_000_001)


def test_inaccessible_identity_still_fails_cleanup_conservatively(monkeypatch):
    def denied(_pid):
        raise psutil.AccessDenied(123)

    monkeypatch.setattr(psutil, "Process", denied)
    assert _daemon_helpers.process_identity_is_alive(123, 1_700_000_000)


def test_shutdown_wait_uses_the_original_verified_process(monkeypatch):
    original = RecordedProcess(psutil.STATUS_SLEEPING)
    waits = []
    original.wait = lambda *, timeout: waits.append(timeout)
    monkeypatch.setattr(psutil, "Process", lambda _pid: original)
    retained = _daemon_helpers._recorded_process(123, 1_700_000_000)
    monkeypatch.setattr(psutil, "Process", lambda _pid: pytest.fail("PID re-opened"))
    _daemon_helpers._wait_for_recorded_process_exit(retained)
    assert waits == [5]


def test_shutdown_never_waits_on_a_recycled_identity(monkeypatch):
    original = RecordedProcess(psutil.STATUS_SLEEPING)
    original.wait = lambda **_kwargs: pytest.fail("waited on recycled PID")
    monkeypatch.setattr(psutil, "Process", lambda _pid: original)
    retained = _daemon_helpers._recorded_process(123, 1_700_000_001)
    assert retained is None
    _daemon_helpers._wait_for_recorded_process_exit(retained)


def test_shutdown_wait_timeout_keeps_the_final_live_check(monkeypatch):
    original = RecordedProcess(psutil.STATUS_SLEEPING)

    def timeout(*, timeout):
        raise psutil.TimeoutExpired(timeout, pid=123)

    original.wait = timeout
    monkeypatch.setattr(psutil, "Process", lambda _pid: original)
    _daemon_helpers._wait_for_recorded_process_exit(original)
    assert _daemon_helpers.process_identity_is_alive(123, 1_700_000_000)


class DeniedCmdline(RecordedProcess):
    def ppid(self) -> int:
        return 1

    def cmdline(self) -> list[str]:
        raise psutil.AccessDenied(123)


def test_snapshot_names_the_field_that_failed(monkeypatch):
    # #1995: a denied cmdline must not hide that status and create_time were
    # readable and matched the recorded identity.
    process = DeniedCmdline(psutil.STATUS_RUNNING)
    monkeypatch.setattr(psutil, "Process", lambda _pid: process)
    monkeypatch.setattr(psutil, "pid_exists", lambda _pid: True)
    snapshot = _daemon_helpers.identity_snapshot(123, 1_700_000_000)
    assert "status='running'" in snapshot
    assert "create_time_int_matches=True" in snapshot
    assert "cmdline=<AccessDenied" in snapshot


def test_wait_outcome_says_how_the_wait_ended():
    assert _daemon_helpers._wait_for_recorded_process_exit(None).startswith("no verified")

    class Slow:
        def wait(self, timeout: float) -> None:
            raise psutil.TimeoutExpired(timeout)

    assert _daemon_helpers._wait_for_recorded_process_exit(Slow()) == "TimeoutExpired after 5s"
