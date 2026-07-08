from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

import pytest

from ._daemon_helpers import (
    DETACH_EXIT_TIMEOUT,
    daemon_env,
    kill_daemon_for_session,
    kill_process,
    launch_daemonized,
    launch_detached,
    managed_env,
    pid_is_alive,
    session_metadata,
    wait_for_exit,
    wait_for_pids_to_exit,
    wait_for_tree_pids,
)

pytestmark = pytest.mark.integration


class TestDaemonCentralizedCleanup:
    def test_subprocess_tree_dies_when_daemon_dies(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        tree_log = tmp_path / "subprocess-tree.jsonl"
        env = daemon_env(mock_env, state_dir)
        proc, session_id = launch_daemonized(
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
        metadata = session_metadata(state_dir, session_id)
        proc.kill()
        proc.wait(timeout=10)

        pids = wait_for_tree_pids(tree_log, 3)

        kill_process(metadata["daemon_pid"])
        wait_for_pids_to_exit(pids)

    @pytest.mark.xfail(
        sys.platform == "win32",
        reason=(
            "Issue #38: mock-agent helper-tree spawn under ConPTY on "
            "Windows records fewer than 3 PIDs — same class of "
            "handle-inheritance / process-spawn quirk as the PTY attach "
            "pipe hang above."
        ),
        strict=True,
    )
    def test_pty_tree_dies_when_daemon_dies(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        tree_log = tmp_path / "pty-tree.jsonl"
        env = daemon_env(mock_env, state_dir)
        proc, session_id = launch_daemonized(
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
        metadata = session_metadata(state_dir, session_id)
        proc.kill()
        proc.wait(timeout=10)

        pids = wait_for_tree_pids(tree_log, 3)

        kill_process(metadata["daemon_pid"])
        wait_for_pids_to_exit(pids)


class TestDaemonSessionHardening:
    def test_detach_prints_attach_hint(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0
            # `read_session_id` already consumed the "session ... running
            # in background" line. The "attach with: clud attach <id>" line
            # follows it, terminated by \n. `readline()` reads until \n so
            # it returns promptly even though the pipe's writer-end is
            # still held open by the detached daemon grandchild (#37). A
            # naive `.read()` would wait forever for EOF.
            assert proc.stderr is not None
            hint_line = proc.stderr.readline()
            assert "attach with: clud attach" in hint_line, f"got: {hint_line!r}"
        finally:
            kill_daemon_for_session(state_dir, session_id)

    def test_attach_no_sessions_shows_helpful_message(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
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
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0
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
            kill_daemon_for_session(state_dir, session_id)

    def test_kill_session(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0
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
            kill_daemon_for_session(state_dir, session_id)

    def test_kill_all_sessions(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        _, session_id_1 = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "kill-all-1",
            "--",
            "--mock-sleep-ms",
            "30000",
        )
        _, session_id_2 = launch_detached(
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
            kill_daemon_for_session(state_dir, session_id_1)

    def test_named_session_attach_by_name(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0

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
            kill_daemon_for_session(state_dir, session_id)

    def test_named_session_shows_in_list(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--name",
            "my-task",
            "--codex",
            "-p",
            "list-name",
            "--",
            "--mock-sleep-ms",
            "10000",
        )
        try:
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0
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
            kill_daemon_for_session(state_dir, session_id)

    def test_prefix_matching_attach(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0
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
            kill_daemon_for_session(state_dir, session_id)

    def test_kill_by_name(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0
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
            kill_daemon_for_session(state_dir, session_id)

    def test_attach_last(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        # Create first session
        proc1, session_id_1 = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "first-session",
            "--",
            "--mock-sleep-ms",
            "5000",
        )
        assert wait_for_exit(proc1, timeout=DETACH_EXIT_TIMEOUT) == 0
        time.sleep(0.2)
        # Create second session (most recent)
        proc2, _session_id_2 = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "second-session",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        assert wait_for_exit(proc2, timeout=DETACH_EXIT_TIMEOUT) == 0
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
            kill_daemon_for_session(state_dir, session_id_1)

    def test_worker_crash_on_startup_records_failure(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # Issue #25: when the backend exits immediately with a non-zero code
        # (simulates a worker startup-time crash), the daemon must still
        # record the actual exit_code in the session snapshot, the worker
        # process must terminate cleanly, the daemon must survive, and the
        # session must NOT show up as "running" in `clud list`.
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "crash-startup",
            "--",
            "--mock-exit-code",
            "17",
            "--mock-sleep-ms",
            "0",
        )
        try:
            assert wait_for_exit(proc, timeout=10) == 0

            # Wait specifically until the worker has BOTH observed the
            # backend exit AND persisted the actual exit_code. We can't use
            # `_wait_for_session_exit` here because that helper returns
            # early when root_pid dies but exit_code hasn't been written
            # yet — and that's exactly the race we need to wait through.
            deadline = time.time() + 15.0
            metadata: dict | None = None
            while time.time() < deadline:
                metadata = session_metadata(state_dir, session_id)
                if metadata["exit_code"] is not None:
                    break
                time.sleep(0.1)
            assert metadata is not None
            assert metadata["exit_code"] == 17, (
                f"expected exit_code=17 from --mock-exit-code 17; got {metadata!r}"
            )

            # Worker should self-terminate after broadcast_exit (its main
            # accept-loop bails when stop_accepting && !has_client).
            worker_pid = metadata["worker_pid"]
            wait_for_pids_to_exit([worker_pid], timeout=10)

            # Daemon must survive — its stale-state cleanup may mark sessions
            # exited, but the daemon process itself must still be alive and
            # serving (otherwise we'd be looking at a much worse bug).
            daemon_pid = metadata["daemon_pid"]
            assert pid_is_alive(daemon_pid), (
                f"daemon process {daemon_pid} died on backend crash"
            )

            # `clud list` must not show this dead session as running.
            listed = subprocess.run(
                [str(clud_binary), "list"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert listed.returncode == 0
            assert session_id not in listed.stdout, (
                f"crashed session still listed as running: {listed.stdout!r}"
            )
        finally:
            kill_daemon_for_session(state_dir, session_id)
