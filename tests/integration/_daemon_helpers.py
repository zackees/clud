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


def attach_for_report(
    clud_binary: Path,
    env: dict[str, str],
    state_dir: Path,
    session_id: str,
    expect: str,
    timeout: float = 30.0,
) -> dict:
    """Return the mock agent's JSON report for `session_id`.

    Issue #595, second attempt. The mock agent writes its report at the *end*
    of its run, so an attach landing before then connects fine, exits 0, and
    replays an empty backlog.

    The first fix retried the attach. That is not enough, and was observed
    failing as `clud attach exited 1 on attempt 2`: retrying only helps while
    the session is still attachable, and the very thing being waited for --
    the agent finishing -- is what makes it *un*-attachable. The race has no
    winning attach schedule, because a fast agent finishes before the first
    attempt and a slow one after the last.

    So attach is the fast path and `clud logs` is the fallback. The session log
    is written to disk and outlives the session, which makes it the only source
    that answers the question in both orderings.

    Failure still separates the cases that mean different things: a report
    found nowhere is reported with the session's recorded `exit_code`, which
    distinguishes "the agent is still running" from "it exited having produced
    nothing".
    """
    deadline = time.time() + timeout
    last_stdout = ""
    last_stderr = ""
    attempts = 0
    while time.time() < deadline:
        attempts += 1
        attached = subprocess.run(
            [str(clud_binary), "attach", session_id],
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        last_stdout, last_stderr = attached.stdout, attached.stderr
        if attached.returncode != 0:
            # Almost always "the session already ended", which is a legitimate
            # ordering rather than an error -- the report is on disk. Only if
            # the log has nothing either is this a real failure.
            report = _report_from_session_log(clud_binary, env, session_id, expect)
            if report is not None:
                return report
            raise AssertionError(
                f"clud attach exited {attached.returncode} on attempt {attempts} "
                f"and the session log holds no report containing {expect!r}\n"
                f"stdout: {attached.stdout!r}\nstderr: {attached.stderr!r}"
            )
        try:
            report = json.loads(attached.stdout)
        except json.JSONDecodeError:
            time.sleep(0.2)
            continue
        if expect in report.get("args", []):
            return report
        time.sleep(0.2)

    # Deadline reached without attach producing it — try the log once more, in
    # case the agent finished during the final sleep.
    report = _report_from_session_log(clud_binary, env, session_id, expect)
    if report is not None:
        return report

    try:
        exit_code = session_metadata(state_dir, session_id).get("exit_code")
    except Exception as err:  # diagnostics only — never mask the real failure
        exit_code = f"<unreadable: {err}>"
    raise AssertionError(
        f"no report containing {expect!r} after {attempts} attach attempt(s) "
        f"in {timeout}s, and none in the session log "
        f"(session exit_code={exit_code})\n"
        f"last stdout: {last_stdout!r}\nlast stderr: {last_stderr!r}"
    )


def _report_from_session_log(
    clud_binary: Path,
    env: dict[str, str],
    session_id: str,
    expect: str,
) -> dict | None:
    """Recover the agent's JSON report from the persistent session log.

    `clud logs` reads the on-disk log, so unlike attach it still works after
    the session has ended — exactly the ordering attach cannot cover.

    Scans line by line for the first JSON object carrying `expect`, rather than
    parsing the whole output: the log also holds ordinary agent chatter, and a
    PTY session's output can be wrapped in ANSI escapes.
    """
    logs = subprocess.run(
        [str(clud_binary), "logs", session_id],
        capture_output=True,
        text=True,
        timeout=15,
        env=env,
    )
    if logs.returncode != 0:
        return None
    for line in strip_ansi(logs.stdout).splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            candidate = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(candidate, dict) and expect in candidate.get("args", []):
            return candidate
    return None


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
