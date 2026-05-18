from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

import pytest

from ._daemon_helpers import (
    daemon_env,
    kill_daemon_for_session,
    kill_process_only,
    launch_daemonized,
    launch_detached,
    managed_env,
    session_metadata,
    strip_ansi,
    wait_for_exit,
    wait_for_pids_to_exit,
)

pytestmark = pytest.mark.integration


class TestDaemonCentralizedPersistence:
    def test_daemon_crash_recovery_kills_worker_and_backend(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "daemon-crash-recovery",
            "--",
            "--mock-sleep-ms",
            "30000",
        )

        wait_for_exit(proc, timeout=10)
        metadata = session_metadata(state_dir, session_id)
        daemon_pid = metadata["daemon_pid"]
        worker_pid = metadata["worker_pid"]
        root_pid = metadata["root_pid"]
        assert root_pid is not None

        kill_process_only(daemon_pid)
        wait_for_pids_to_exit([worker_pid, root_pid], timeout=15)

    def test_subprocess_session_persists_and_reattaches(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        env = daemon_env(mock_env, tmp_path / "daemon-state")
        proc, session_id = launch_daemonized(
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

    @pytest.mark.xfail(
        sys.platform == "win32",
        reason=(
            "Issue #38: `clud attach <id>` times out on Windows PTY "
            "sessions — the attach subprocess runs to completion but Python's "
            "communicate() never sees pipe EOF. Handle-inheritance guard in "
            "PR #39 didn't clear it; deeper Windows ConPTY / CreateProcess "
            "investigation needed."
        ),
        strict=True,
    )
    def test_pty_session_persists_and_reattaches(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        env = daemon_env(mock_env, tmp_path / "daemon-state")
        proc, session_id = launch_daemonized(
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
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        assert result.returncode == 0
        cleaned = strip_ansi(result.stdout).strip()
        report = json.loads(cleaned.splitlines()[-1])
        assert "hello-pty" in report["args"]

    @pytest.mark.xfail(
        sys.platform == "win32",
        reason="Issue #38 — same Windows PTY attach pipe-EOF bug as above.",
        strict=True,
    )
    def test_pty_attach_replay_paints_current_frame(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # Issue #34: mid-session attach on a PTY session must replay the
        # current frame, not a raw byte dump. Paint a known TUI frame via
        # mock-agent's --mock-ansi-script, detach, reattach, assert both
        # markers are in the attach's output (they are absolute-positioned,
        # so a working replay places them at the right cells).
        paint_path = tmp_path / "paint.ansi"
        paint_path.write_bytes(
            b"\x1b[2J\x1b[1;1H__HEADER__\x1b[5;1H__FOOTER__"
        )
        env = daemon_env(mock_env, tmp_path / "daemon-state")
        proc, session_id = launch_daemonized(
            clud_binary,
            env,
            "--pty",
            "-p",
            "paint-test",
            "--",
            "--mock-ansi-script",
            str(paint_path),
            "--mock-sleep-ms",
            "3000",
        )
        # Give the PTY worker time to ingest the paint bytes into its
        # TerminalCapture before we disconnect. Without this, the reattach
        # might win the race and see an empty grid.
        time.sleep(0.5)
        proc.kill()
        proc.wait(timeout=10)

        result = subprocess.run(
            [str(clud_binary), "attach", session_id],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        assert result.returncode == 0
        cleaned = strip_ansi(result.stdout)
        assert "__HEADER__" in cleaned, (
            f"HEADER missing from attach replay: {cleaned!r}"
        )
        assert "__FOOTER__" in cleaned, (
            f"FOOTER missing from attach replay: {cleaned!r}"
        )

    def test_clud_logs_dumps_session_log(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # pm2-style: after a session runs, `clud logs <id>` should print the
        # captured output from the persistent log file, independent of
        # whether any client is still attached.
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "logme-tag",
            "--",
            "--mock-sleep-ms",
            "500",
        )
        try:
            assert wait_for_exit(proc, timeout=5) == 0
            # Give the worker a beat to finish appending to the log file.
            time.sleep(0.6)

            result = subprocess.run(
                [str(clud_binary), "logs", session_id],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert result.returncode == 0, result.stderr
            # The mock agent writes a JSON report to stdout with the args it
            # received; our prompt-tag must appear in the captured log.
            assert "logme-tag" in result.stdout, (
                f"prompt tag missing from logs output: {result.stdout!r}"
            )
        finally:
            kill_daemon_for_session(state_dir, session_id)

    def test_clud_logs_last_and_follow(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # Issue #25: `clud logs --last` must resolve to the most recent
        # session (including exited ones) and `-f` must tail without
        # crashing, then exit cleanly once the session is dead.
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "tail-me-tag",
            "--",
            "--mock-sleep-ms",
            "400",
        )
        try:
            assert wait_for_exit(proc, timeout=5) == 0
            time.sleep(0.6)

            # `--last` should pick the session we just ran (the only one).
            last_result = subprocess.run(
                [str(clud_binary), "logs", "--last"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert last_result.returncode == 0, last_result.stderr
            assert "tail-me-tag" in last_result.stdout, (
                f"prompt tag missing from --last output: {last_result.stdout!r}"
            )
            assert session_id in last_result.stderr, (
                f"--last hint should mention resolved session id: {last_result.stderr!r}"
            )
            # Since the session is already dead, the status line should fire.
            assert "exited with status" in last_result.stderr, (
                f"expected exit-status line on dead session: {last_result.stderr!r}"
            )

            # Follow mode on a dead session: prints backlog, drains, then
            # exits 0 once it spots the recorded exit_code in the snapshot.
            follow_result = subprocess.run(
                [str(clud_binary), "logs", "-f", session_id],
                capture_output=True,
                text=True,
                timeout=15,
                env=env,
            )
            assert follow_result.returncode == 0, follow_result.stderr
            assert "tail-me-tag" in follow_result.stdout, (
                f"prompt tag missing from -f output: {follow_result.stdout!r}"
            )
            assert "exited with status" in follow_result.stderr, (
                f"follow mode must announce exit: {follow_result.stderr!r}"
            )
        finally:
            kill_daemon_for_session(state_dir, session_id)

    def test_clud_logs_with_no_id_lists_sessions(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "list-me",
            "--",
            "--mock-sleep-ms",
            "400",
        )
        try:
            assert wait_for_exit(proc, timeout=5) == 0
            time.sleep(0.5)

            result = subprocess.run(
                [str(clud_binary), "logs"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert result.returncode == 0, result.stderr
            assert session_id in result.stdout, (
                f"session id missing from summary: {result.stdout!r}"
            )
        finally:
            kill_daemon_for_session(state_dir, session_id)

    def test_daemon_reboot_purges_orphaned_session_state(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # Issue #25: when both the daemon and worker die uncleanly (e.g. host
        # reboot, OOM kill), the on-disk session snapshot is left with
        # `exit_code = None`, which would make `clud list` report a phantom
        # "running" session. The next daemon boot's `cleanup_stale_state()`
        # must mark such sessions exited so the world is consistent again.
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "orphan-test",
            "--",
            "--mock-sleep-ms",
            "30000",
        )
        wait_for_exit(proc, timeout=10)

        # Snapshot is populated; capture all relevant pids while they're alive.
        metadata = session_metadata(state_dir, session_id)
        daemon_pid = metadata["daemon_pid"]
        worker_pid = metadata["worker_pid"]
        root_pid = metadata["root_pid"]
        assert metadata["exit_code"] is None

        # Sanity: orphan session shows up as "running" in `clud list` before reboot.
        listed_before = subprocess.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert listed_before.returncode == 0
        assert session_id in listed_before.stdout

        # Kill daemon FIRST, so we shrink the window in which its
        # liveness-monitor would self-clean the worker. We then race to kill
        # the worker before its 200ms-tick monitor notices.
        kill_process_only(daemon_pid)
        kill_process_only(worker_pid)
        if root_pid is not None:
            kill_process_only(root_pid)
        wait_for_pids_to_exit([worker_pid] + ([root_pid] if root_pid else []))

        # Snapshot file should still exist on disk (nothing removed it; we
        # killed the worker before its `fs::remove_file(spec_path)` line).
        snapshot_path = state_dir / "sessions" / f"{session_id}.json"
        assert snapshot_path.is_file(), "snapshot must persist across reboot"

        # Now boot a fresh daemon by spawning a brand-new session. The new
        # parent process calls `ensure_daemon()` -> `cleanup_stale_state()`
        # before the new daemon child starts.
        proc2, session_id_2 = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "post-reboot",
            "--",
            "--mock-sleep-ms",
            "3000",
        )
        try:
            wait_for_exit(proc2, timeout=10)

            # The orphan's snapshot must now have an exit_code recorded
            # (cleanup_stale_state sets 137 for sessions with dead workers).
            orphan_meta = json.loads(
                snapshot_path.read_text(encoding="utf-8")
            )
            assert orphan_meta["exit_code"] is not None, (
                f"orphan still has exit_code=None after daemon reboot: {orphan_meta}"
            )

            # And `clud list` must no longer show the orphan as running.
            listed_after = subprocess.run(
                [str(clud_binary), "list"],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert listed_after.returncode == 0
            assert session_id not in listed_after.stdout, (
                f"orphan session still listed after reboot: {listed_after.stdout!r}"
            )
        finally:
            kill_daemon_for_session(state_dir, session_id_2)

    def test_stale_attached_client_evicted_by_heartbeat(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # Issue #25: a half-closed TCP attach client (terminal crash, SSH
        # drop, kill -9) used to permanently block reattach with "session
        # already has an attached client". The daemon's 2s heartbeat thread
        # now probes the attach socket and evicts dead peers. A fresh
        # attach must succeed within ~heartbeat-interval seconds.
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        # Use a backend that lives long enough to outlive the eviction
        # heartbeat (2s) plus our buffer — but short enough that the
        # second attach can wait it out and exit cleanly within timeout.
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "stale-attach",
            "--",
            "--mock-sleep-ms",
            "8000",
        )
        first_attach: subprocess.Popen[str] | None = None
        try:
            wait_for_exit(proc, timeout=10)

            first_attach = subprocess.Popen(
                [str(clud_binary), "attach", session_id],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=env,
            )
            # Give the attach a beat to handshake with the worker before we
            # forcibly kill it. Without this we might kill the client process
            # before the worker has registered it, so eviction wouldn't be
            # the path under test.
            time.sleep(1.0)
            assert first_attach.poll() is None, (
                f"attach client died before we could kill it: "
                f"stderr={first_attach.stderr.read() if first_attach.stderr else ''!r}"
            )

            # Force-kill (no graceful shutdown) — leaves a half-closed TCP
            # socket on the daemon's side until the heartbeat probes it.
            kill_process_only(first_attach.pid)
            first_attach.wait(timeout=5)

            # Heartbeat fires every 2s (see daemon.rs:1627). Wait long
            # enough for at least one probe to detect the dead peer and
            # call `evict_dead_client`. We also rely on `attach_client`
            # itself calling `evict_dead_client` first (daemon.rs:399), so
            # this is a belt-and-suspenders timeout.
            time.sleep(3.0)

            # Second attach: must NOT be rejected as occupied. The mock
            # backend has ~4s of sleep remaining, after which it exits 0
            # and the attach drains and returns.
            second_attach = subprocess.run(
                [str(clud_binary), "attach", session_id],
                capture_output=True,
                text=True,
                timeout=20,
                env=env,
            )
            assert "session already has an attached client" not in second_attach.stderr, (
                f"stale client was not evicted; stderr={second_attach.stderr!r}"
            )
            assert second_attach.returncode == 0, (
                f"reattach failed: rc={second_attach.returncode} "
                f"stdout={second_attach.stdout!r} stderr={second_attach.stderr!r}"
            )
        finally:
            if first_attach is not None and first_attach.poll() is None:
                first_attach.kill()
                first_attach.wait(timeout=5)
            kill_daemon_for_session(state_dir, session_id)
