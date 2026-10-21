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
