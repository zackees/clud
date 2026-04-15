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


def _managed_env(mock_env: dict[str, str], state_dir: Path) -> dict[str, str]:
    env = mock_env.copy()
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    return env


def _extract_session_id(line: str) -> str | None:
    """Extract session id from various stderr formats."""
    # "[clud] daemon session sess-XXX"
    if "daemon session" in line:
        return line.strip().rsplit(" ", 1)[-1]
    # "[clud] session sess-XXX running in background"
    if "session" in line and "running in background" in line:
        return line.strip().split("session ", 1)[-1].split(" running")[0]
    return None


def _read_session_id(proc: subprocess.Popen[str], timeout: float = 10.0) -> str:
    assert proc.stderr is not None
    deadline = time.time() + timeout
    while time.time() < deadline:
        line = proc.stderr.readline()
        session_id = _extract_session_id(line)
        if session_id is not None:
            return session_id
        if proc.poll() is not None:
            raise AssertionError(f"clud exited early while waiting for session id: {line!r}")
    raise AssertionError("timed out waiting for daemon session id")


def _read_session_id_from_text(stderr: str) -> str:
    for line in stderr.splitlines():
        session_id = _extract_session_id(line)
        if session_id is not None:
            return session_id
    raise AssertionError(f"daemon session id not found in stderr: {stderr!r}")


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


def _session_metadata(state_dir: Path, session_id: str) -> dict:
    path = state_dir / "sessions" / f"{session_id}.json"
    _wait_for_file(path)
    return json.loads(path.read_text(encoding="utf-8"))


def _wait_for_session_exit(state_dir: Path, session_id: str, timeout: float = 15.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        metadata = _session_metadata(state_dir, session_id)
        if metadata["exit_code"] is not None:
            return metadata
        root_pid = metadata.get("root_pid")
        if root_pid is not None and not _pid_is_alive(root_pid):
            return metadata
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for session {session_id} to exit")


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


def _launch_detached(
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
    session_id = _read_session_id(proc)
    return proc, session_id


def _wait_for_exit(proc: subprocess.Popen[str], timeout: float = 10.0) -> int:
    return proc.wait(timeout=timeout)


def _kill_daemon_for_session(state_dir: Path, session_id: str) -> None:
    metadata = _session_metadata(state_dir, session_id)
    _kill_process(metadata["daemon_pid"])


class TestDaemonManagedSessionFlags:
    def test_detach_launch_returns_immediately_and_can_attach(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "hello-detach",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0

            attached = subprocess.run(
                [str(clud_binary), "attach", session_id],
                capture_output=True,
                text=True,
                timeout=15,
                env=env,
            )
            assert attached.returncode == 0
            report = json.loads(attached.stdout)
            assert "hello-detach" in report["args"]
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_attach_without_session_id_lists_attachable_sessions(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        launch_cwd = tmp_path / "workspace"
        launch_cwd.mkdir()
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "list-attachable",
            "--",
            "--mock-sleep-ms",
            "3000",
            cwd=launch_cwd,
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0

            listed = subprocess.run(
                [str(clud_binary), "attach"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert listed.returncode == 0
            assert session_id in listed.stdout
            assert str(launch_cwd) in listed.stdout
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_list_shows_attachable_pid_and_cwd(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        launch_cwd = tmp_path / "workspace"
        launch_cwd.mkdir()
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "list-session",
            "--",
            "--mock-sleep-ms",
            "3000",
            cwd=launch_cwd,
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0
            metadata = _session_metadata(state_dir, session_id)

            listed = subprocess.run(
                [str(clud_binary), "list"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert listed.returncode == 0
            assert session_id in listed.stdout
            assert str(metadata["root_pid"]) in listed.stdout
            assert str(launch_cwd) in listed.stdout
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_detachable_ctrl_c_yes_backgrounds_session(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        kwargs: dict[str, object] = {}
        if sys.platform == "win32":
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP

        proc = subprocess.Popen(
            [
                str(clud_binary),
                "--detachable",
                "--codex",
                "-p",
                "hello-detachable",
                "--",
                "--mock-sleep-ms",
                "5000",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.PIPE,
            text=True,
            env=env,
            **kwargs,
        )

        try:
            session_id = _read_session_id(proc)
            listed = subprocess.run(
                [str(clud_binary), "list"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert listed.returncode == 0
            assert session_id not in listed.stdout

            time.sleep(0.5)
            if sys.platform == "win32":
                proc.send_signal(signal.CTRL_BREAK_EVENT)
            else:
                proc.send_signal(signal.SIGINT)
            assert proc.stdin is not None
            proc.stdin.write("y\n")
            proc.stdin.flush()
            _wait_for_exit(proc, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)

        assert proc.returncode == 0

        metadata = _session_metadata(state_dir, session_id)
        assert metadata["exit_code"] is None
        assert metadata["root_pid"] is not None
        assert _pid_is_alive(metadata["root_pid"])
        listed = subprocess.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert listed.returncode == 0
        assert session_id in listed.stdout

        try:
            attached = subprocess.run(
                [str(clud_binary), "attach", session_id],
                capture_output=True,
                text=True,
                timeout=15,
                env=env,
            )
            assert attached.returncode == 0
            report = json.loads(attached.stdout)
            assert "hello-detachable" in report["args"]
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_detachable_ctrl_c_timeout_ends_session(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        kwargs: dict[str, object] = {}
        if sys.platform == "win32":
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP

        proc = subprocess.Popen(
            [
                str(clud_binary),
                "--detachable",
                "--codex",
                "-p",
                "hello-timeout",
                "--",
                "--mock-sleep-ms",
                "30000",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.PIPE,
            text=True,
            env=env,
            **kwargs,
        )

        try:
            session_id = _read_session_id(proc)
            time.sleep(0.5)
            if sys.platform == "win32":
                proc.send_signal(signal.CTRL_BREAK_EVENT)
            else:
                proc.send_signal(signal.SIGINT)
            _wait_for_exit(proc, timeout=15)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)

        if sys.platform == "win32":
            assert proc.returncode in (130, 3221225786)
        else:
            assert proc.returncode == 130

        metadata = _wait_for_session_exit(state_dir, session_id, timeout=12.0)
        assert metadata["exit_code"] is not None or not _pid_is_alive(metadata["root_pid"])

        listed = subprocess.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert listed.returncode == 0
        assert session_id not in listed.stdout


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


class TestDaemonSessionHardening:
    def test_detach_prints_attach_hint(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "hint-test",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0
            stderr = proc.stderr.read() if proc.stderr else ""
            assert "attach with: clud attach" in stderr
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_attach_no_sessions_shows_helpful_message(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        result = subprocess.run(
            [str(clud_binary), "attach"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert result.returncode == 0
        assert "No active sessions" in result.stdout

    def test_attach_auto_attaches_single_session(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "auto-attach",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0
            attached = subprocess.run(
                [str(clud_binary), "attach"],
                capture_output=True,
                text=True,
                timeout=15,
                env=env,
            )
            assert attached.returncode == 0
            report = json.loads(attached.stdout)
            assert "auto-attach" in report["args"]
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_kill_session(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "kill-me",
            "--",
            "--mock-sleep-ms",
            "30000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0
            result = subprocess.run(
                [str(clud_binary), "kill", session_id],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert result.returncode == 0
            assert "killed session" in result.stderr

            listed = subprocess.run(
                [str(clud_binary), "list"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert session_id not in listed.stdout
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_kill_all_sessions(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        _, session_id_1 = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "kill-all-1",
            "--",
            "--mock-sleep-ms",
            "30000",
        )
        _, session_id_2 = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "kill-all-2",
            "--",
            "--mock-sleep-ms",
            "30000",
        )
        try:
            result = subprocess.run(
                [str(clud_binary), "kill", "--all"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert result.returncode == 0

            listed = subprocess.run(
                [str(clud_binary), "list"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert session_id_1 not in listed.stdout
            assert session_id_2 not in listed.stdout
        finally:
            _kill_daemon_for_session(state_dir, session_id_1)

    def test_named_session_attach_by_name(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--name",
            "my-refactor",
            "--codex",
            "-p",
            "named-test",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0

            # Attach by name instead of ID
            attached = subprocess.run(
                [str(clud_binary), "attach", "my-refactor"],
                capture_output=True,
                text=True,
                timeout=15,
                env=env,
            )
            assert attached.returncode == 0
            report = json.loads(attached.stdout)
            assert "named-test" in report["args"]
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_named_session_shows_in_list(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--name",
            "my-task",
            "--codex",
            "-p",
            "list-name",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0
            listed = subprocess.run(
                [str(clud_binary), "list"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert listed.returncode == 0
            assert "my-task" in listed.stdout
            assert session_id in listed.stdout
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_prefix_matching_attach(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "prefix-test",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0
            # Use first 10 chars of session_id as prefix
            prefix = session_id[:10]
            attached = subprocess.run(
                [str(clud_binary), "attach", prefix],
                capture_output=True,
                text=True,
                timeout=15,
                env=env,
            )
            assert attached.returncode == 0
            report = json.loads(attached.stdout)
            assert "prefix-test" in report["args"]
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_kill_by_name(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        proc, session_id = _launch_detached(
            clud_binary,
            env,
            "--name",
            "kill-by-name",
            "--codex",
            "-p",
            "kill-name-test",
            "--",
            "--mock-sleep-ms",
            "30000",
        )
        try:
            assert _wait_for_exit(proc, timeout=2) == 0
            result = subprocess.run(
                [str(clud_binary), "kill", "kill-by-name"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert result.returncode == 0
            assert "killed session" in result.stderr
        finally:
            _kill_daemon_for_session(state_dir, session_id)

    def test_attach_last(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = _managed_env(mock_env, state_dir)
        # Create first session
        proc1, session_id_1 = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "first-session",
            "--",
            "--mock-sleep-ms",
            "5000",
        )
        assert _wait_for_exit(proc1, timeout=2) == 0
        time.sleep(0.2)
        # Create second session (most recent)
        proc2, _session_id_2 = _launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "second-session",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        assert _wait_for_exit(proc2, timeout=2) == 0
        try:
            # attach --last should get the second session
            attached = subprocess.run(
                [str(clud_binary), "attach", "--last"],
                capture_output=True,
                text=True,
                timeout=15,
                env=env,
            )
            assert attached.returncode == 0
            report = json.loads(attached.stdout)
            assert "second-session" in report["args"]
        finally:
            _kill_daemon_for_session(state_dir, session_id_1)
