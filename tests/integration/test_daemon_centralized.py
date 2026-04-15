from __future__ import annotations

import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

pytestmark = pytest.mark.integration

_ANSI_RE = re.compile(r"\x1b(?:\[[^a-zA-Z]*[a-zA-Z]|\][^\x07]*\x07)")


def _daemon_env(mock_env: dict[str, str], state_dir: Path) -> dict[str, str]:
    env = mock_env.copy()
    env["CLUD_EXPERIMENTAL_DAEMON"] = "1"
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    return env


def _read_session_id(proc: subprocess.Popen[str], timeout: float = 10.0) -> str:
    assert proc.stderr is not None
    deadline = time.time() + timeout
    while time.time() < deadline:
        line = proc.stderr.readline()
        if "daemon session" in line:
            return line.strip().rsplit(" ", 1)[-1]
        if proc.poll() is not None:
            raise AssertionError(f"clud exited early while waiting for session id: {line!r}")
    raise AssertionError("timed out waiting for daemon session id")


def _wait_for_file(path: Path, timeout: float = 10.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if path.is_file():
            return
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {path}")


def _strip_ansi(text: str) -> str:
    return _ANSI_RE.sub("", text)


def _wait_for_tree_pids(path: Path, minimum: int, timeout: float = 10.0) -> list[int]:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if path.is_file():
            pids = [
                json.loads(line)["pid"]
                for line in path.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            if len(pids) >= minimum:
                return pids
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {minimum} tree pids in {path}")


def _session_metadata(state_dir: Path, session_id: str) -> dict:
    path = state_dir / "sessions" / f"{session_id}.json"
    _wait_for_file(path)
    return json.loads(path.read_text(encoding="utf-8"))


def _kill_process(pid: int) -> None:
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/T", "/F"],
            capture_output=True,
            text=True,
            check=False,
        )
    else:
        os.kill(pid, signal.SIGKILL)


def _pid_is_alive(pid: int) -> bool:
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


def _wait_for_pids_to_exit(pids: list[int], timeout: float = 15.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not any(_pid_is_alive(pid) for pid in pids):
            return
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for pids to exit: {pids}")


def _launch_daemonized(
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
    session_id = _read_session_id(proc)
    return proc, session_id


class TestDaemonCentralizedPersistence:
    def test_subprocess_session_persists_and_reattaches(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        env = _daemon_env(mock_env, tmp_path / "daemon-state")
        proc, session_id = _launch_daemonized(
            clud_binary,
            env,
            "--codex",
            "-p",
            "hello",
            "--",
            "--mock-sleep-ms",
            "3000",
        )

        proc.kill()
        proc.wait(timeout=10)

        result = subprocess.run(
            [str(clud_binary), "attach", session_id],
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        assert result.returncode == 0
        report = json.loads(result.stdout)
        assert "hello" in report["args"]

    def test_pty_session_persists_and_reattaches(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        env = _daemon_env(mock_env, tmp_path / "daemon-state")
        proc, session_id = _launch_daemonized(
            clud_binary,
            env,
            "--pty",
            "-p",
            "hello-pty",
            "--",
            "--mock-sleep-ms",
            "3000",
        )

        proc.kill()
        proc.wait(timeout=10)

        result = subprocess.run(
            [str(clud_binary), "attach", session_id],
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        assert result.returncode == 0
        cleaned = _strip_ansi(result.stdout).strip()
        report = json.loads(cleaned.splitlines()[-1])
        assert "hello-pty" in report["args"]


class TestDaemonCentralizedCleanup:
    def test_subprocess_tree_dies_when_daemon_dies(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        tree_log = tmp_path / "subprocess-tree.jsonl"
        env = _daemon_env(mock_env, state_dir)
        proc, session_id = _launch_daemonized(
            clud_binary,
            env,
            "--codex",
            "-p",
            "tree",
            "--",
            "--mock-sleep-ms",
            "15000",
            "--mock-spawn-tree-log",
            str(tree_log),
        )
        metadata = _session_metadata(state_dir, session_id)
        proc.kill()
        proc.wait(timeout=10)

        pids = _wait_for_tree_pids(tree_log, 3)

        _kill_process(metadata["daemon_pid"])
        _wait_for_pids_to_exit(pids)

    def test_pty_tree_dies_when_daemon_dies(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        tree_log = tmp_path / "pty-tree.jsonl"
        env = _daemon_env(mock_env, state_dir)
        proc, session_id = _launch_daemonized(
            clud_binary,
            env,
            "--pty",
            "-p",
            "tree-pty",
            "--",
            "--mock-sleep-ms",
            "15000",
            "--mock-spawn-tree-log",
            str(tree_log),
        )
        metadata = _session_metadata(state_dir, session_id)
        proc.kill()
        proc.wait(timeout=10)

        pids = _wait_for_tree_pids(tree_log, 3)

        _kill_process(metadata["daemon_pid"])
        _wait_for_pids_to_exit(pids)
