from __future__ import annotations

import json
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from ._daemon_helpers import (
    DETACH_EXIT_TIMEOUT,
    daemon_env,
    kill_daemon_for_session,
    launch_detached,
    managed_env,
    pid_is_alive,
    read_session_id,
    read_session_id_from_text,
    session_metadata,
    wait_for_exit,
    wait_for_file,
)

pytestmark = pytest.mark.integration


class TestDaemonManagedSessionFlags:
    def test_transcript_flag_forces_daemon_and_writes_output(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        transcript = tmp_path / "session.transcript"
        env = managed_env(mock_env, state_dir)

        result = subprocess.run(
            [
                str(clud_binary),
                "--transcript",
                str(transcript),
                "--codex",
                "-p",
                "transcript-tag",
                "--",
                "--mock-sleep-ms",
                "1000",
            ],
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        assert result.returncode == 0, result.stderr
        session_id = read_session_id_from_text(result.stderr)
        wait_for_file(transcript)
        contents = transcript.read_text(encoding="utf-8")
        assert "transcript-tag" in contents
        kill_daemon_for_session(state_dir, session_id)

    def test_detach_launch_returns_immediately_and_can_attach(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0

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
            kill_daemon_for_session(state_dir, session_id)

    def test_attach_without_session_id_lists_attachable_sessions(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        launch_cwd = tmp_path / "workspace"
        launch_cwd.mkdir()
        # Create two sessions so attach (no args) lists instead of auto-attaching
        proc1, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "list-attachable",
            "--",
            "--mock-sleep-ms",
            "5000",
            cwd=launch_cwd,
        )
        _proc2, _session_id_2 = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "list-attachable-2",
            "--",
            "--mock-sleep-ms",
            "5000",
        )
        try:
            assert wait_for_exit(proc1, timeout=DETACH_EXIT_TIMEOUT) == 0

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
            kill_daemon_for_session(state_dir, session_id)

    def test_list_shows_attachable_pid_and_cwd(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        launch_cwd = tmp_path / "workspace"
        launch_cwd.mkdir()
        proc, session_id = launch_detached(
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
            assert wait_for_exit(proc, timeout=DETACH_EXIT_TIMEOUT) == 0
            metadata = session_metadata(state_dir, session_id)

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
            kill_daemon_for_session(state_dir, session_id)

    def test_detachable_ctrl_c_yes_backgrounds_session(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
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
            session_id = read_session_id(proc)
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
            wait_for_exit(proc, timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)

        assert proc.returncode == 0

        metadata = session_metadata(state_dir, session_id)
        assert metadata["exit_code"] is None
        assert metadata["root_pid"] is not None
        assert pid_is_alive(metadata["root_pid"])
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
            kill_daemon_for_session(state_dir, session_id)

    def test_detachable_noninteractive_ctrl_c_backgrounds_session(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # Issue #25: a non-interactive attach has no safe prompt surface.
        # Ctrl+C should detach/background immediately instead of treating
        # arbitrary piped stdin as a yes/no answer.
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
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
            session_id = read_session_id(proc)
            time.sleep(0.5)
            if sys.platform == "win32":
                proc.send_signal(signal.CTRL_BREAK_EVENT)
            else:
                proc.send_signal(signal.SIGINT)
            wait_for_exit(proc, timeout=15)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)

        stderr_tail = proc.stderr.read() if proc.stderr else ""
        assert proc.returncode == 0, (
            f"expected 0 (backgrounded) got {proc.returncode}; stderr={stderr_tail}"
        )
        assert "non-interactive attach interrupted" in stderr_tail

        # Session metadata must still reflect a live worker.
        metadata = session_metadata(state_dir, session_id)
        assert metadata["exit_code"] is None
        assert metadata["root_pid"] is not None
        assert pid_is_alive(metadata["root_pid"])

        # `clud list` must show the backgrounded session.
        listed = subprocess.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert listed.returncode == 0
        assert session_id in listed.stdout

        kill_daemon_for_session(state_dir, session_id)

    def test_foreground_ctrl_c_fast_paths_daemon_kill_and_profiles(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        errors: list[str] = []
        attempts = 5 if sys.platform == "win32" else 1

        for attempt in range(attempts):
            state_dir = tmp_path / f"daemon-state-{attempt}"
            env = daemon_env(mock_env, state_dir)
            kwargs: dict[str, object] = {}
            if sys.platform == "win32":
                kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP

            proc = subprocess.Popen(
                [
                    str(clud_binary),
                    "--codex",
                    "-p",
                    "fast-ctrl-c",
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

            session_id = ""
            try:
                session_id = read_session_id(proc)
                time.sleep(0.5)
                started = time.perf_counter()
                if sys.platform == "win32":
                    proc.send_signal(signal.CTRL_BREAK_EVENT)
                else:
                    proc.send_signal(signal.SIGINT)
                proc.wait(timeout=3.0)
                elapsed = time.perf_counter() - started
                if proc.returncode != 130:
                    errors.append(f"attempt {attempt}: returncode {proc.returncode}")
                    continue
                if elapsed >= 2.0:
                    errors.append(f"attempt {attempt}: handoff took {elapsed:.3f}s")
                    continue

                deadline = time.time() + 10.0
                metadata = session_metadata(state_dir, session_id)
                profile = metadata.get("ctrl_c") or {}
                while time.time() < deadline:
                    metadata = session_metadata(state_dir, session_id)
                    profile = metadata.get("ctrl_c") or {}
                    if (
                        metadata.get("exit_code") == 130
                        and profile.get("daemon_kill_ms") is not None
                    ):
                        break
                    time.sleep(0.1)
                else:
                    errors.append(
                        f"attempt {attempt}: daemon profile incomplete: {metadata!r}"
                    )
                    continue

                assert profile.get("fast_path") is True
                cli_handoff_ms = profile.get("cli_handoff_ms")
                if cli_handoff_ms is not None:
                    assert isinstance(cli_handoff_ms, int)
                    assert cli_handoff_ms < 2000
                else:
                    # The foreground fast path has two valid telemetry
                    # shapes: a client interrupt request with CLI timing,
                    # or a daemon/worker-side kill profile when Windows
                    # delivers the interrupt through the child tree first.
                    # The user-visible contract is the already-measured
                    # fast return plus daemon-side kill timing below.
                    assert profile.get("daemon_kill_ms") is not None
                assert isinstance(profile.get("daemon_kill_ms"), int)
                return
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait(timeout=5)
                if session_id:
                    kill_daemon_for_session(state_dir, session_id)

        raise AssertionError("Ctrl-C daemon fast path did not complete: " + "; ".join(errors))

    def test_loop_repeat_registers_background_job_and_lists_status(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)

        result = subprocess.run(
            [
                str(clud_binary),
                "loop",
                "--loop-count",
                "1",
                "--repeat",
                "1s",
                "repeat background task",
                "--",
                "--mock-sleep-ms",
                "50",
            ],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
            cwd=tmp_path,
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"
        session_id = read_session_id_from_text(result.stderr)

        deadline = time.time() + 10
        while time.time() < deadline:
            metadata = session_metadata(state_dir, session_id)
            if metadata["repeat_interval_secs"] == 1:
                break
            time.sleep(0.05)
        else:
            raise AssertionError(f"repeat metadata not populated: {metadata}")

        listed = subprocess.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
            cwd=tmp_path,
        )
        assert listed.returncode == 0, f"stderr: {listed.stderr}"
        assert session_id in listed.stdout
        assert "repeat background task" in listed.stdout
        assert ("running" in listed.stdout) or ("sleeping" in listed.stdout)

        kill_daemon_for_session(state_dir, session_id)

    def test_concurrent_attach_attempt_is_rejected(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        proc, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "concurrent-attach",
            "--",
            "--mock-sleep-ms",
            "30000",
        )

        first_attach = subprocess.Popen(
            [str(clud_binary), "attach", session_id],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )

        try:
            wait_for_exit(proc, timeout=10)
            time.sleep(1.0)
            assert first_attach.poll() is None

            second_attach = subprocess.run(
                [str(clud_binary), "attach", session_id],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            assert second_attach.returncode != 0
            assert "session already has an attached client" in second_attach.stderr
        finally:
            if first_attach.poll() is None:
                first_attach.kill()
                first_attach.wait(timeout=5)
            kill_daemon_for_session(state_dir, session_id)

    def test_detachable_noninteractive_ctrl_c_ignores_piped_n(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        # Issue #25: there is no safe prompt surface when stdin/stderr are
        # pipes, so piped bytes must not be interpreted as background-prompt
        # answers. Even a queued "n" should detach/background immediately.
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        kwargs: dict[str, object] = {}
        if sys.platform == "win32":
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP

        proc = subprocess.Popen(
            [
                str(clud_binary),
                "--detachable",
                "--codex",
                "-p",
                "hello-explicit-no",
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
            session_id = read_session_id(proc)
            time.sleep(0.5)
            if sys.platform == "win32":
                proc.send_signal(signal.CTRL_BREAK_EVENT)
            else:
                proc.send_signal(signal.SIGINT)
            assert proc.stdin is not None
            proc.stdin.write("n\n")
            proc.stdin.flush()
            wait_for_exit(proc, timeout=15)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)

        stderr_tail = proc.stderr.read() if proc.stderr else ""
        assert proc.returncode == 0, (
            f"expected 0 (backgrounded) got {proc.returncode}; stderr={stderr_tail}"
        )
        assert "non-interactive attach interrupted" in stderr_tail

        metadata = session_metadata(state_dir, session_id)
        assert metadata["exit_code"] is None
        assert metadata["root_pid"] is not None
        assert pid_is_alive(metadata["root_pid"])

        listed = subprocess.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert listed.returncode == 0
        assert session_id in listed.stdout

        kill_daemon_for_session(state_dir, session_id)
