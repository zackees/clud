"""#1738: a recycled parent PID must not drag an older process into a tree kill.

Windows never rewrites a process's parent PID when the parent exits, and it
recycles PIDs. A process whose parent died therefore looks like a child of
whatever later process is handed that PID. The integration suite's daemon
cleanup (`_daemon_helpers.kill_process`) tree-kills a clud daemon by PID; when
the daemon was handed the PID of a dead ancestor of the pytest runner, the walk
went *up* into the runner and killed pytest itself (exit 1, no summary).
"""

from __future__ import annotations

import os
import sys
import time

import psutil
import pytest

from tests import process

from ._daemon_helpers import kill_process

pytestmark = [
    pytest.mark.integration,
    pytest.mark.skipif(
        sys.platform != "win32",
        reason="POSIX re-parents orphans, so a parent PID cannot go stale",
    ),
]

_RECYCLE_BUDGET_SECS = 45.0


def _ancestry() -> str:
    """Describe this process's ancestor chain, flagging dead-parent links."""
    lines = []
    try:
        current: psutil.Process | None = psutil.Process(os.getpid())
    except psutil.Error as error:
        return f"<ancestry unavailable: {error!r}>"
    seen: set[int] = set()
    while current is not None and current.pid not in seen:
        seen.add(current.pid)
        try:
            name = current.name()
            created = current.create_time()
            ppid = current.ppid()
        except psutil.Error as error:
            lines.append(f"  pid={current.pid} <{error!r}>")
            break
        try:
            parent = psutil.Process(ppid) if ppid else None
            parent_created = parent.create_time() if parent is not None else None
        except psutil.Error:
            parent, parent_created = None, None
        if parent is None:
            link = "DEAD parent"
        elif parent_created is not None and parent_created > created:
            link = "parent PID RECYCLED"
            parent = None
        else:
            link = "live parent"
        lines.append(f"  pid={current.pid} {name} created={created:.3f} ppid={ppid} ({link})")
        current = parent
    return "\n".join(lines)


def _wait_until(predicate, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return predicate()


def _orphan_with_stale_parent() -> tuple[process.RunningChild, int, psutil.Process]:
    """Leave a ping.exe whose parent PID names an exited cmd.exe.

    Returns `(keeper, dead parent PID, orphan)`. The outer cmd.exe (the
    keeper) runs `start /b cmd /c start /b ping` and then a ping of its own,
    so it stays alive: running-process holds each child in a kill-on-close
    job, and dropping a child that exited would take the orphan with it. The
    inner cmd.exe exits at once, and nothing of ours ever held a handle to it,
    so its PID is free to be recycled.
    """
    started = time.time()
    keeper = process.Popen(
        [
            "cmd", "/d", "/c",
            "start", "/b", "cmd", "/d", "/c", "start", "/b", "ping", "-n", "120", "127.0.0.1",
            ">NUL", "2>&1", "&",
            "ping", "-n", "120", "127.0.0.1", ">NUL", "2>&1",
        ],
    )
    orphan: list[psutil.Process] = []

    def find() -> bool:
        for candidate in psutil.process_iter(["ppid", "name", "create_time"]):
            info = candidate.info
            if (
                (info["name"] or "").lower() == "ping.exe"
                and (info["create_time"] or 0) >= started - 1
                and info["ppid"] not in (None, keeper.pid)
                and not psutil.pid_exists(info["ppid"])
            ):
                orphan.append(candidate)
                return True
        return False

    if not _wait_until(find, 10):
        keeper.kill()
        tree = [
            f"{p.pid}:{p.info['name']}<-{p.info['ppid']}"
            for p in psutil.process_iter(["ppid", "name", "create_time"])
            if (p.info["create_time"] or 0) >= started - 1
        ]
        pytest.fail(f"no ping.exe with an exited parent appeared; recent processes: {tree}")
    return keeper, orphan[0].ppid(), orphan[0]


def _recycle(pid: int) -> process.RunningChild | None:
    """Spawn processes until one is handed `pid`; release the others at once."""
    deadline = time.monotonic() + _RECYCLE_BUDGET_SECS
    while time.monotonic() < deadline:
        candidate = process.Popen(
            ["ping", "-n", "120", "127.0.0.1"],
            stdout=process.PIPE,
            stderr=process.PIPE,
        )
        if candidate.pid == pid:
            return candidate
        candidate.kill()
        _wait_until(lambda candidate=candidate: candidate.poll() is not None, 5)
        del candidate
    return None


def test_tree_kill_spares_older_process_whose_parent_pid_was_recycled() -> None:
    keeper, stale_parent, orphan = _orphan_with_stale_parent()
    orphan_created = orphan.create_time()
    holder = None
    try:
        holder = _recycle(stale_parent)
        if holder is None:
            pytest.skip(f"PID {stale_parent} was not recycled within {_RECYCLE_BUDGET_SECS}s")
        # Precondition: the holder is younger than the orphan that names it.
        assert psutil.Process(stale_parent).create_time() > orphan_created

        kill_process(stale_parent)
        time.sleep(0.5)

        assert orphan.is_running(), (
            f"tree kill of recycled PID {stale_parent} killed older PID {orphan.pid} "
            f"(ping.exe, created {orphan_created:.3f}), which only names it as parent "
            "because Windows recycled the PID.\n"
            f"pytest ancestry:\n{_ancestry()}"
        )
    finally:
        if holder is not None and holder.poll() is None:
            holder.kill()
        try:
            orphan.kill()
        except psutil.Error:
            pass
        process.terminate_process_tree(keeper.pid)
