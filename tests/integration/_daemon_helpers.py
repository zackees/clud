"""Shared helpers for the daemon-mode integration test modules.

These were factored out of `test_daemon_centralized.py` when that file was
split into per-test-class modules to keep individual files under the 1k-LOC
threshold (after which the AI gets stuck reading a single source file).
The leading underscore in the filename keeps pytest from collecting it as
a test module.
"""

from __future__ import annotations

import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path


def run_clud(
    argv: list[str],
    *,
    timeout: float,
    **kwargs,
) -> subprocess.CompletedProcess[str]:
    """`subprocess.run` that reports what the process managed to do before it
    timed out.

    Issue #594: the Windows x86 lane fails ~every run with a rotating single
    victim, most of them a `clud.exe` launch exceeding its budget. A bare
    `TimeoutExpired` names only the command and the number of seconds, so
    every occurrence looks identical and none says *where* the launch stalled
    — which is precisely that issue's open question ("genuinely slow under
    load, or intermittently blocking?").

    Re-raising with that output, plus the measured elapsed time, turns each
    occurrence into evidence: startup chatter that stops mid-daemon-bringup
    reads very differently from a child that produced nothing at all.

    Note the deliberate avoidance of `subprocess.run(..., timeout=)`. On
    Windows — the only platform where #594 fires — CPython raises
    `TimeoutExpired` from the reader-thread path *without* attaching the
    partial buffers, so `expired.stdout` is empty there while being populated
    on POSIX. Reading them requires killing the child and draining the pipes
    ourselves, which is what this does. Verified rather than assumed: the
    first version of this helper used `subprocess.run` and reported nothing at
    all on Windows.

    Deliberately changes no timeout value. Whether these budgets are right is
    the question; widening them would answer it by erasing it.
    """
    started = time.monotonic()
    proc = subprocess.Popen(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        stdin=subprocess.PIPE if kwargs.get("input") is not None else None,
        text=True,
        **{k: v for k, v in kwargs.items() if k != "input"},
    )
    try:
        stdout, stderr = proc.communicate(input=kwargs.get("input"), timeout=timeout)
    except subprocess.TimeoutExpired as expired:
        elapsed = time.monotonic() - started
        proc.kill()
        # Second communicate() drains what the reader threads already buffered.
        try:
            stdout, stderr = proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            stdout, stderr = "<pipes unreadable after kill>", ""
        raise AssertionError(
            f"timed out after {timeout}s (waited {elapsed:.1f}s): {argv}\n"
            f"--- partial stdout ---\n{stdout}\n"
            f"--- partial stderr ---\n{stderr}"
        ) from expired
    return subprocess.CompletedProcess(argv, proc.returncode, stdout, stderr)


_ANSI_RE = re.compile(
    # CSI: \x1b[ + params + final letter
    r"\x1b(?:\[[^a-zA-Z]*[a-zA-Z]"
    # OSC: \x1b] + string + BEL or ST (\x1b\\)
    r"|\][^\x07]*(?:\x07|\x1b\\)"
    # Bare ESC + single printable byte. Covers RIS (\x1bc), keypad
    # normal/application (\x1b=, \x1b>), save/restore cursor (\x1b7, \x1b8),
    # index / reverse index / next line (\x1bD, \x1bM, \x1bE), etc.
    # Issue #34: attach-replay snapshot emits these so the client's terminal
    # restores full state; the test must strip them before parsing JSON.
    r"|[\x30-\x7e])"
)
DETACH_EXIT_TIMEOUT = 10.0


def daemon_env(mock_env: dict[str, str], state_dir: Path) -> dict[str, str]:
    env = mock_env.copy()
    env["CLUD_EXPERIMENTAL_DAEMON"] = "1"
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    return env


def managed_env(mock_env: dict[str, str], state_dir: Path) -> dict[str, str]:
    env = mock_env.copy()
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    return env


def extract_session_id(line: str) -> str | None:
    """Extract session id from various stderr formats."""
    # "[clud] daemon session sess-XXX"
    if "daemon session" in line:
        return line.strip().rsplit(" ", 1)[-1]
    # "[clud] session sess-XXX running in background"
    if "session" in line and "running in background" in line:
        return line.strip().split("session ", 1)[-1].split(" running")[0]
    # "[clud] repeat job sess-XXX running in background"
    if "repeat job" in line and "running in background" in line:
        return line.strip().split("repeat job ", 1)[-1].split(" running")[0]
    return None


def read_session_id(proc: subprocess.Popen[str], timeout: float = 10.0) -> str:
    assert proc.stderr is not None
    deadline = time.time() + timeout
    while time.time() < deadline:
        line = proc.stderr.readline()
        session_id = extract_session_id(line)
        if session_id is not None:
            return session_id
        if proc.poll() is not None:
            raise AssertionError(f"clud exited early while waiting for session id: {line!r}")
    raise AssertionError("timed out waiting for daemon session id")


def read_session_id_from_text(stderr: str) -> str:
    for line in stderr.splitlines():
        session_id = extract_session_id(line)
        if session_id is not None:
            return session_id
    raise AssertionError(f"daemon session id not found in stderr: {stderr!r}")


def wait_for_file(path: Path, timeout: float = 10.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if path.is_file():
            return
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {path}")


def strip_ansi(text: str) -> str:
    return _ANSI_RE.sub("", text)


def wait_for_tree_pids(path: Path, minimum: int, timeout: float = 10.0) -> list[int]:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if path.is_file():
            pids = []
            for line in path.read_text(encoding="utf-8").splitlines():
                if not line.strip():
                    continue
                try:
                    pids.append(json.loads(line)["pid"])
                except json.JSONDecodeError:
                    continue
            if len(pids) >= minimum:
                return pids
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {minimum} tree pids in {path}")


def session_metadata(
    state_dir: Path, session_id: str, timeout: float = 5.0
) -> dict:
    path = state_dir / "sessions" / f"{session_id}.json"
    wait_for_file(path)
    deadline = time.time() + timeout
    while True:
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except (PermissionError, json.JSONDecodeError):
            if time.time() >= deadline:
                raise
            time.sleep(0.05)


def wait_for_session_exit(state_dir: Path, session_id: str, timeout: float = 15.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        metadata = session_metadata(state_dir, session_id)
        if metadata["exit_code"] is not None:
            return metadata
        root_pid = metadata.get("root_pid")
        if root_pid is not None and not pid_is_alive(root_pid):
            return metadata
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for session {session_id} to exit")


def kill_process(pid: int) -> None:
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/T", "/F"],
            capture_output=True,
            text=True,
            check=False,
        )
    else:
        os.kill(pid, signal.SIGKILL)


def kill_process_only(pid: int) -> None:
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/F"],
            capture_output=True,
            text=True,
            check=False,
        )
    else:
        os.kill(pid, signal.SIGKILL)


def pid_is_alive(pid: int) -> bool:
    if sys.platform == "win32":
        result = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"],
            capture_output=True,
            text=True,
            check=False,
        )
        return f'"{pid}"' in result.stdout or f",{pid}," in result.stdout
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def wait_for_pids_to_exit(pids: list[int], timeout: float = 15.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not any(pid_is_alive(pid) for pid in pids):
            return
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for pids to exit: {pids}")


def launch_daemonized(
    clud_binary: Path,
    env: dict[str, str],
    *args: str,
) -> tuple[subprocess.Popen[str], str]:
    proc = subprocess.Popen(
        [str(clud_binary), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    session_id = read_session_id(proc)
    return proc, session_id


def launch_detached(
    clud_binary: Path,
    env: dict[str, str],
    *args: str,
    cwd: Path | None = None,
) -> tuple[subprocess.Popen[str], str]:
    proc = subprocess.Popen(
        [str(clud_binary), "--detach", *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
        cwd=cwd,
    )
    session_id = read_session_id(proc)
    return proc, session_id


def wait_for_exit(proc: subprocess.Popen[str], timeout: float = 10.0) -> int:
    """Wait for `proc`, reporting what it had emitted if it overruns.

    Issue #594: `Popen.wait` captures nothing, so the two integration victims
    that time out here (`TimeoutExpired([...clud.exe, --detach, ...], 5)` and
    the 30 s concurrent-launch one) said only that a launch was too slow —
    never how far it got. Drain the pipes on overrun instead.

    The process is killed first so `communicate` cannot block behind a child
    that is still running; a second short timeout guards even that, since a
    wedged process is one of the outcomes being investigated.
    """
    started = time.monotonic()
    try:
        return proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired as expired:
        elapsed = time.monotonic() - started
        proc.kill()
        try:
            stdout, stderr = proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            stdout, stderr = "<pipes unreadable after kill>", ""
        raise AssertionError(
            f"process did not exit within {timeout}s (waited {elapsed:.1f}s)\n"
            f"--- partial stdout ---\n{stdout}\n"
            f"--- partial stderr ---\n{stderr}"
        ) from expired


def kill_daemon_for_session(state_dir: Path, session_id: str) -> None:
    path = state_dir / "sessions" / f"{session_id}.json"
    if not path.is_file():
        return
    try:
        metadata = session_metadata(state_dir, session_id)
    except FileNotFoundError:
        return
    kill_process(metadata["daemon_pid"])
