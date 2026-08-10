"""Shared helpers for the daemon-mode integration test modules.

These were factored out of `test_daemon_centralized.py` when that file was
split into per-test-class modules to keep individual files under the 1k-LOC
threshold (after which the AI gets stuck reading a single source file).
The leading underscore in the filename keeps pytest from collecting it as
a test module.
"""

from __future__ import annotations

import contextlib
import json
import os
import re
import shutil
import signal
import sys
import tempfile
import time
from collections.abc import Callable
from pathlib import Path

import psutil

from tests import process


def copy_launcher(src: Path, dest: Path, *, attempts: int = 12, delay: float = 0.25) -> Path:
    """Copy a clud/mock binary to ``dest``, retrying a transient Windows
    Access-denied.

    Issue #594: on the Windows lanes, ``shutil.copy2`` of a freshly-written
    ``.exe`` intermittently fails with ``PermissionError: [WinError 5]`` when
    Defender (or the search indexer) opens the just-created file for scanning
    and briefly holds an exclusive handle. The lock clears within a fraction of
    a second, so a short bounded retry turns the rotating single-victim copy
    failure into a non-event. On POSIX the first copy succeeds, so this is a
    no-op there.
    """
    last_err: OSError | None = None
    for _ in range(attempts):
        try:
            shutil.copy2(src, dest)
            return dest
        except PermissionError as err:  # WinError 5 on Windows
            last_err = err
            time.sleep(delay)
    assert last_err is not None
    raise last_err


def run_clud(
    argv: list[str],
    *,
    timeout: float,
    **kwargs,
) -> process.CompletedProcess[str]:
    """`process.run` that reports what the process managed to do before it
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

    Note the deliberate avoidance of `process.run(..., timeout=)`. On
    Windows — the only platform where #594 fires — CPython raises
    `TimeoutExpired` from the reader-thread path *without* attaching the
    partial buffers, so `expired.stdout` is empty there while being populated
    on POSIX. Reading them requires killing the child and draining the pipes
    ourselves, which is what this does. Verified rather than assumed: the
    first version of this helper used `process.run` and reported nothing at
    all on Windows.

    Deliberately changes no timeout value. Whether these budgets are right is
    the question; widening them would answer it by erasing it.
    """
    started = time.monotonic()
    # Exit-stage attribution, via a file rather than stderr.
    #
    # The partial-stderr capture below proved the payload completes and the
    # process then fails to *exit* in budget, which moved #594's open question
    # to "what holds it open after the work is done". clud's own exit-timing
    # summary cannot answer that: it prints after the last stage, so a process
    # we kill mid-teardown emits nothing. The per-stage breadcrumbs go to this
    # file as they happen, so a `begin` with no matching `done` names the
    # culprit even though the process never finished.
    #
    # A file, not `CLUD_EXIT_TIMING=1`, because that variable routes to stderr
    # and would break every test that asserts on clean stderr.
    env = dict(kwargs.pop("env", None) or os.environ)
    trace_fd, trace_path = tempfile.mkstemp(prefix="clud-exit-stage-", suffix=".log")
    os.close(trace_fd)
    env["CLUD_EXIT_TIMING_FILE"] = trace_path

    proc = process.Popen(
        argv,
        stdout=process.PIPE,
        stderr=process.PIPE,
        stdin=process.PIPE if kwargs.get("input") is not None else None,
        text=True,
        env=env,
        **{k: v for k, v in kwargs.items() if k != "input"},
    )
    try:
        stdout, stderr = proc.communicate(input=kwargs.get("input"), timeout=timeout)
    except process.TimeoutExpired as expired:
        elapsed = time.monotonic() - started
        proc.kill()
        # Second communicate() drains what the reader threads already buffered.
        try:
            stdout, stderr = proc.communicate(timeout=5)
        except process.TimeoutExpired:
            stdout, stderr = "<pipes unreadable after kill>", ""
        raise AssertionError(
            f"timed out after {timeout}s (waited {elapsed:.1f}s): {argv}\n"
            f"--- partial stdout ---\n{stdout}\n"
            f"--- partial stderr ---\n{stderr}\n"
            f"--- exit stages ---\n{_read_exit_stages(trace_path)}"
        ) from expired
    finally:
        with contextlib.suppress(OSError):
            os.unlink(trace_path)
    return process.CompletedProcess(argv, proc.returncode, stdout, stderr)


def _read_exit_stages(path: str) -> str:
    """Render the exit-stage trace, calling out an unterminated stage.

    An unmatched `begin` is the whole point of the file: it is the stage the
    process was still inside when the harness killed it.
    """
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            lines = [line.rstrip("\n") for line in handle if line.strip()]
    except OSError as err:
        return f"<unreadable: {err}>"
    if not lines:
        return "<none recorded — never reached the exit path>"
    started = [ln.split()[-1] for ln in lines if ln.startswith("exit-stage begin ")]
    finished = [ln.split()[-1].split("=")[0] for ln in lines if ln.startswith("exit-stage done ")]
    rendered = "\n".join(lines)
    stuck = [name for name in started if name not in finished]
    if stuck:
        rendered += f"\n>>> STALLED IN: {', '.join(stuck)} (entered, never completed)"
    return rendered


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
    env = managed_env(mock_env, state_dir)
    env["CLUD_EXPERIMENTAL_DAEMON"] = "1"
    return env


def managed_env(mock_env: dict[str, str], state_dir: Path) -> dict[str, str]:
    env = mock_env.copy()
    for name in (
        "CLUD_DAEMON_TEST_MAX_LIFETIME_SECS",
        "CLUD_DAEMON_TEST_IDLE_TIMEOUT_SECS",
        "CLUD_DAEMON_TEST_HOST_SCANS",
    ):
        env.pop(name, None)
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    env["CLUD_DAEMON_TEST_MODE"] = "1"
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


def read_session_id(proc: process.Popen[str], timeout: float = 10.0) -> str:
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


def session_metadata(state_dir: Path, session_id: str, timeout: float = 5.0) -> dict:
    path = state_dir / "sessions" / f"{session_id}.json"
    wait_for_file(path)
    deadline = time.time() + timeout
    while True:
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except (FileNotFoundError, PermissionError, json.JSONDecodeError):
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
    attach_args: list[str] | None = None,
) -> dict:
    """Return the mock agent's JSON report for `session_id`.

    `attach_args` overrides what is passed to `clud attach` (default
    `[session_id]`) so callers that exercise a *selector* — by name, by id
    prefix, `--last`, or no argument at all — get the same race handling
    without giving up the thing they are testing. `session_id` is still
    required, because the `clud logs` fallback below needs the exact session
    even when the attach under test addressed it indirectly.

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
        attached = process.run(
            # `is None`, not `or`: an empty list is a meaningful selector --
            # bare `clud attach` with no argument -- and `or` would silently
            # substitute the session id, testing the opposite of the intent.
            [
                str(clud_binary),
                "attach",
                *([session_id] if attach_args is None else attach_args),
            ],
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
    logs = process.run(
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


def run_until(
    argv: list[str],
    env: dict[str, str],
    ready: Callable[[process.CompletedProcess[str]], bool],
    *,
    what: str,
    deadline_secs: float = 20.0,
    interval: float = 0.1,
    timeout: float = 10.0,
) -> process.CompletedProcess[str]:
    """Re-run `argv` until `ready` accepts its result, then return that result.

    Issue #718. A daemon-facing read (`clud logs --last`, `clud logs`,
    `clud logs <id>`) can race the daemon's own persistence of a session that
    has only just exited: the command answers honestly that it sees nothing, a
    moment before the write lands. Tests used to bridge that with a fixed
    `time.sleep(0.6)`, which is a guess at the window — and on a loaded macOS
    runner the guess loses, surfacing as `[clud] no sessions found`.

    Polling the real condition removes the guess without slowing the passing
    case: the common path succeeds on the first attempt and never sleeps.

    `ready` takes the whole `CompletedProcess` rather than just the exit code
    because the interesting precondition often is not the exit code. `clud
    logs <id>` exits 0 as soon as the session record exists, which can be
    before the worker has finished appending the log body the caller wants to
    assert on — so that site asks for the content, not the status.

    On timeout the last observed stdout/stderr is included: a bare "timed out"
    from a flaky-test fix would be strictly worse than the sleep it replaced.
    """
    deadline = time.time() + deadline_secs
    attempts = 0
    result: process.CompletedProcess[str] | None = None
    while True:
        result = process.run(
            argv,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
        attempts += 1
        if ready(result):
            return result
        if time.time() >= deadline:
            break
        time.sleep(interval)
    last_rc = result.returncode if result else None
    last_stdout = result.stdout if result else ""
    last_stderr = result.stderr if result else ""
    raise AssertionError(
        f"timed out after {deadline_secs}s ({attempts} attempts) waiting for {what}\n"
        f"  argv={argv!r}\n"
        f"  last returncode={last_rc}\n"
        f"  last stdout={last_stdout!r}\n"
        f"  last stderr={last_stderr!r}"
    )


def kill_process(pid: int) -> None:
    if sys.platform == "win32":
        process.terminate_process_tree(pid)
    else:
        os.kill(pid, signal.SIGKILL)


def kill_process_only(pid: int) -> None:
    try:
        psutil.Process(pid).kill()
    except psutil.NoSuchProcess:
        pass


def pid_is_alive(pid: int) -> bool:
    if sys.platform == "win32":
        result = process.run(
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


def process_identity_is_alive(pid: int, start_time: int) -> bool:
    """Return whether the exact recorded process identity is still alive.

    DaemonInfo.pid_start uses sysinfo's seconds-since-epoch start time.  Pairing
    it with the PID prevents a recycled PID from making teardown target an
    unrelated process.
    """
    try:
        process = psutil.Process(pid)
        if not process.is_running():
            return False
        if start_time <= 0:
            raise ValueError("process identity requires a positive start time")
        return int(process.create_time()) == start_time
    except (psutil.NoSuchProcess, psutil.ZombieProcess):
        return False
    except psutil.AccessDenied:
        # Be conservative: if the OS will not let us verify the recorded
        # identity, teardown must report a failure instead of guessing.
        return True


def _daemon_cleanup_diagnostics(
    state_dir: Path,
    *,
    pid: int | None,
    start_time: int | None,
    result: process.CompletedProcess[str] | None = None,
    error: BaseException | None = None,
) -> str:
    details = [f"state_dir: {state_dir}"]
    if pid is not None:
        details.append(f"recorded identity: pid={pid}, start_time={start_time}")
        if start_time is not None and start_time > 0 and process_identity_is_alive(pid, start_time):
            try:
                command_line = " ".join(psutil.Process(pid).cmdline())
            except (psutil.Error, OSError):
                command_line = "<unavailable>"
            details.append(f"live command line: {command_line}")
    if result is not None:
        details.extend(
            [
                f"stop return code: {result.returncode}",
                f"stop stdout: {result.stdout!r}",
                f"stop stderr: {result.stderr!r}",
            ]
        )
    if error is not None:
        details.append(f"stop error: {error!r}")

    events_path = state_dir / "daemon-events.jsonl"
    if events_path.is_file():
        try:
            events = events_path.read_text(encoding="utf-8").splitlines()[-20:]
            details.append("daemon event tail:\n" + "\n".join(events))
        except (OSError, UnicodeError) as event_error:
            details.append(f"daemon event tail unreadable: {event_error!r}")
    else:
        details.append("daemon event tail: <missing>")
    return "\n".join(details)


def stop_daemon(
    clud_binary: Path,
    state_dir: Path,
    base_env: dict[str, str] | None = None,
) -> None:
    """Stop and verify the daemon recorded in ``state_dir``.

    The public command owns shutdown identity validation.  This helper only
    snapshots the identity first so teardown can prove that exact process is
    gone, without a PID-only kill fallback.
    """
    info_path = state_dir / "daemon.json"
    pid: int | None = None
    start_time: int | None = None
    if info_path.is_file():
        try:
            info = json.loads(info_path.read_text(encoding="utf-8"))
            pid = int(info["pid"])
            start_time = int(info["pid_start"])
            if start_time <= 0:
                raise ValueError("pid_start must be positive")
        except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
            raise AssertionError(
                "cannot read daemon identity before cleanup\n"
                + _daemon_cleanup_diagnostics(
                    state_dir,
                    pid=pid,
                    start_time=start_time,
                    error=error,
                )
            ) from error

    env = dict(base_env or os.environ)
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    try:
        result = process.run(
            [str(clud_binary), "daemon", "stop"],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )
    except (OSError, process.TimeoutExpired) as error:
        raise AssertionError(
            "daemon cleanup command failed\n"
            + _daemon_cleanup_diagnostics(
                state_dir,
                pid=pid,
                start_time=start_time,
                error=error,
            )
        ) from error

    identity_alive = (
        pid is not None and start_time is not None and process_identity_is_alive(pid, start_time)
    )
    if result.returncode != 0 or identity_alive or info_path.exists():
        raise AssertionError(
            "daemon cleanup did not complete safely\n"
            + _daemon_cleanup_diagnostics(
                state_dir,
                pid=pid,
                start_time=start_time,
                result=result,
            )
        )


def stop_daemons_below(root: Path, clud_binary: Path) -> list[Path]:
    """Stop every daemon whose state file exists strictly below ``root``."""
    state_dirs = sorted(
        {info_path.parent for info_path in root.rglob("daemon.json")},
        key=lambda path: str(path),
    )
    failures = []
    for state_dir in state_dirs:
        try:
            stop_daemon(clud_binary, state_dir)
        except AssertionError as error:
            failures.append(f"{state_dir}:\n{error}")
    if failures:
        raise AssertionError("one or more daemon cleanups failed:\n\n" + "\n\n".join(failures))
    return state_dirs


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
) -> tuple[process.Popen[str], str]:
    proc = process.Popen(
        [str(clud_binary), *args],
        stdout=process.PIPE,
        stderr=process.PIPE,
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
) -> tuple[process.Popen[str], str]:
    proc = process.Popen(
        [str(clud_binary), "--detach", *args],
        stdout=process.PIPE,
        stderr=process.PIPE,
        text=True,
        env=env,
        cwd=cwd,
    )
    session_id = read_session_id(proc)
    return proc, session_id


def wait_for_exit(proc: process.Popen[str], timeout: float = 10.0) -> int:
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
    except process.TimeoutExpired as expired:
        elapsed = time.monotonic() - started
        proc.kill()
        try:
            stdout, stderr = proc.communicate(timeout=5)
        except process.TimeoutExpired:
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
