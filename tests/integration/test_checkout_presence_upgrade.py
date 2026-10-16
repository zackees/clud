"""Old session vs. upgraded daemon: checkout presence must give up (#1981).

A real `clud` session holds a checkout-presence connection to its daemon.
The daemon is then replaced by a *newer* build, which refuses this older
client for the rest of its life. Before #1981 the session's presence thread
retried twice a second forever, each attempt scanning the whole process
table, so every pre-upgrade session burned about 25% of a core.

The newer daemon is a stand-in that publishes a higher `version` in
`daemon.json` and counts the connections it receives. That count is exactly
the number of reconnect probes, because presence is the only part of an
idle session that probes the daemon.
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

import psutil
import pytest

from tests import process

from ._daemon_helpers import (
    kill_process_only,
    managed_env,
    wait_for_pids_to_exit,
)

pytestmark = pytest.mark.integration

_NOTE = "checkout presence disabled"
# Four of the old 500 ms retry intervals, plus margin.
_QUIET_WINDOW_SECS = 3.0
# An idle session in a busy CI container stays far below this. The pre-fix
# loop spent about 25% of a core on a host with ~5,500 processes.
_MAX_IDLE_CPU_FRACTION = 0.10


def _clud(clud_binary: Path, env: dict[str, str], cwd: Path, *argv: str) -> str:
    result = process.run(
        [str(clud_binary), *argv],
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
        cwd=str(cwd),
    )
    return result.stdout


def _wait_for_presence(
    clud_binary: Path, env: dict[str, str], repo: Path, session: process.Popen
) -> None:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        assert session.poll() is None, f"session exited early: {session.returncode}"
        if f"pid={session.pid} " in _clud(clud_binary, env, repo, "claim", "who"):
            return
        time.sleep(0.2)
    raise AssertionError("session never registered checkout presence")


def _start_newer_daemon(tmp_path: Path) -> tuple[process.Popen, int, Path]:
    """A listener that stands in for the upgraded daemon and counts probes."""
    port_file = tmp_path / "newer-daemon.port"
    count_file = tmp_path / "newer-daemon.probes"
    daemon = process.Popen(
        [
            sys.executable,
            "-c",
            "import socket,sys,pathlib\n"
            "listener=socket.socket()\n"
            "listener.bind(('127.0.0.1',0))\n"
            "listener.listen(64)\n"
            "count=pathlib.Path(sys.argv[2])\n"
            "count.write_text('0')\n"
            "pathlib.Path(sys.argv[1]).write_text(str(listener.getsockname()[1]))\n"
            "probes=0\n"
            "while True:\n"
            "    conn,_=listener.accept()\n"
            "    conn.close()\n"
            "    probes+=1\n"
            "    count.write_text(str(probes))\n",
            str(port_file),
            str(count_file),
        ],
        stdout=process.DEVNULL,
        stderr=process.DEVNULL,
    )
    deadline = time.monotonic() + 10
    while not port_file.exists() and time.monotonic() < deadline:
        time.sleep(0.05)
    assert port_file.exists(), "stand-in daemon failed to start"
    return daemon, int(port_file.read_text()), count_file


def _probes(count_file: Path) -> int:
    return int(count_file.read_text() or "0")


def _drain_stderr(session: process.Popen, lines: list[str], seconds: float) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            line = session.next_stderr(timeout=0.2)
        except TimeoutError:
            continue
        if not isinstance(line, str):
            return
        lines.append(line)


def _cpu_seconds(pid: int) -> float:
    times = psutil.Process(pid).cpu_times()
    return times.user + times.system


def test_old_session_disables_presence_when_a_newer_daemon_takes_over(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    process.run(["git", "init", "-q", str(repo)], check=True, timeout=30)
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)

    session = process.Popen(
        [
            str(clud_binary),
            "--codex",
            "-p",
            "presence-upgrade",
            "--",
            "--mock-sleep-ms",
            "120000",
        ],
        cwd=str(repo),
        env=env,
        stdin=process.DEVNULL,
        stdout=process.DEVNULL,
        stderr=process.PIPE,
        text=True,
    )
    newer_daemon = None
    old_daemon_pid = None
    stderr: list[str] = []
    try:
        _wait_for_presence(clud_binary, env, repo, session)
        info_path = state_dir / "daemon.json"
        old_daemon_pid = int(json.loads(info_path.read_text(encoding="utf-8"))["pid"])

        # The upgrade. Publish the newer daemon first, then remove the old
        # one, so the session's reconnect sees the newer build rather than an
        # empty state dir it would fill with a fresh daemon of its own version.
        newer_daemon, port, count_file = _start_newer_daemon(tmp_path)
        assert newer_daemon.pid is not None
        info_path.write_text(
            json.dumps(
                {
                    "pid": newer_daemon.pid,
                    "pid_start": int(psutil.Process(newer_daemon.pid).create_time()),
                    "port": port,
                    "version": "999.0.0",
                }
            ),
            encoding="utf-8",
        )
        kill_process_only(old_daemon_pid)
        wait_for_pids_to_exit([old_daemon_pid], timeout=15)

        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and not any(_NOTE in line for line in stderr):
            _drain_stderr(session, stderr, 0.5)
        assert any(_NOTE in line for line in stderr), (
            "presence never disabled itself after the upgrade:\n" + "".join(stderr)
        )

        probes_at_note = _probes(count_file)
        cpu_before = _cpu_seconds(session.pid)
        _drain_stderr(session, stderr, _QUIET_WINDOW_SECS)
        cpu_used = _cpu_seconds(session.pid) - cpu_before
        probes_after_note = _probes(count_file) - probes_at_note

        assert session.poll() is None, "the session must keep running without presence"
        notes = [line for line in stderr if _NOTE in line]
        assert len(notes) == 1, f"expected one note, got {len(notes)}:\n" + "".join(stderr)
        assert "999.0.0" in notes[0], notes[0]
        assert probes_after_note == 0, (
            f"the session kept probing the newer daemon: {probes_after_note} probes "
            f"in {_QUIET_WINDOW_SECS}s after the note"
        )
        assert cpu_used <= _MAX_IDLE_CPU_FRACTION * _QUIET_WINDOW_SECS, (
            f"session used {cpu_used:.2f}s of CPU in {_QUIET_WINDOW_SECS}s after the note"
        )
    finally:
        if session.poll() is None:
            process.terminate_process_tree(session.pid)
        if newer_daemon is not None and newer_daemon.poll() is None:
            newer_daemon.kill()
            newer_daemon.wait(timeout=10)
        if old_daemon_pid is not None:
            kill_process_only(old_daemon_pid)
