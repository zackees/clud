"""#1738: a recycled parent PID must not drag an older process into a tree kill.

Windows never rewrites a process's parent PID when the parent exits, and it
recycles PIDs. A process whose parent died therefore looks like a child of
whatever later process is handed that PID. The integration suite's daemon
cleanup (`_daemon_helpers.kill_process`) tree-kills a clud daemon by PID; when
the daemon was handed the PID of a dead ancestor of the pytest runner, the walk
went *up* into the runner and killed pytest itself (exit 1, no summary).
"""

from __future__ import annotations

import gc
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


def _launch_and_exit() -> int:
    """Run `cmd /c start /b ping`, wait for cmd.exe to exit, return its PID.

    The child object goes out of scope on return, closing our handle on the
    dead cmd.exe: Windows keeps a PID reserved while any handle is open.
    """
    # No captured pipes: ping would inherit them, and draining them to EOF
    # would wait for ping. Its output goes to NUL instead.
    launcher = process.Popen(
        # One string, handed to CreateProcess as-is, so cmd parses the redirect.
        'cmd /d /c start "" /b ping -n 120 127.0.0.1 >NUL 2>&1',
    )
    pid = launcher.pid
    assert pid is not None
    assert _wait_until(lambda: launcher.poll() is not None, 10), "cmd.exe launcher did not exit"
    return pid


def _orphan_with_stale_parent() -> tuple[int, psutil.Process]:
    """Start ping.exe through a cmd.exe that then exits; return (dead ppid, ping)."""
    stale_parent = _launch_and_exit()
    gc.collect()
    orphan: list[psutil.Process] = []

    def find() -> bool:
        for candidate in psutil.process_iter(["ppid", "name"]):
            if candidate.info["ppid"] == stale_parent and (
                (candidate.info["name"] or "").lower() == "ping.exe"
            ):
                orphan.append(candidate)
                return True
        return False

    if not _wait_until(find, 10):
        survivors = [
            f"{p.pid}:{p.info['name']}"
            for p in psutil.process_iter(["ppid", "name"])
            if p.info["ppid"] == stale_parent
        ]
        pytest.fail(
            "ping.exe started by `start /b` never appeared; "
            f"children of {stale_parent}: {survivors}"
        )
    return stale_parent, orphan[0]


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
    stale_parent, orphan = _orphan_with_stale_parent()
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
