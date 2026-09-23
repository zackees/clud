from __future__ import annotations

import json
import signal
import sys
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from tests import process

from . import _daemon_helpers
from ._daemon_helpers import (
    DETACH_EXIT_TIMEOUT,
    attach_for_report,
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


def wait_for_ctrl_c_profile(
    read_metadata: Callable[[], dict], timeout: float = 10.0
) -> tuple[dict, dict] | None:
    """Poll one nonblocking metadata read per turn within a single deadline."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            metadata = read_metadata()
        except (FileNotFoundError, PermissionError, json.JSONDecodeError):
            # A snapshot can be replaced or retired while the worker exits.
            # Keep probing until the same deadline used for profile completion.
            pass
        else:
            profile = metadata.get("ctrl_c") or {}
            if metadata.get("exit_code") == 130 and profile.get("daemon_kill_ms") is not None:
                return metadata, profile
        time.sleep(min(0.1, max(0.0, deadline - time.monotonic())))
    return None


def read_ctrl_c_metadata_once(state_dir: Path, session_id: str) -> dict:
    path = state_dir / "sessions" / f"{session_id}.json"
    return json.loads(path.read_text(encoding="utf-8"))


def safe_daemon_event_tail(path: Path, session_id: str) -> str:
    """Read at most 8 KiB and expose only bounded lifecycle fields."""
    limit = 8192
    with path.open("rb") as handle:
        handle.seek(0, 2)
        size = handle.tell()
        handle.seek(max(0, size - limit))
        tail = handle.read(limit)
    if size > limit:
        # The first record may start before the bounded window.
        tail = tail.partition(b"\n")[2]

    safe_lines = []
    for line in tail.splitlines()[-10:]:
        try:
            event = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError):
            continue
        if not isinstance(event, dict):
            continue
        safe: dict[str, int | str | bool] = {}
        for key in ("ts_ms", "event_id", "daemon_pid", "pid", "worker_pid", "exit_code"):
            value = event.get(key)
            if type(value) is int and 0 <= value <= 10**16:
                safe[key] = value
        op = event.get("op")
        if isinstance(op, str) and 0 < len(op) <= 48 and op.isascii() and all(
            char.islower() or char.isdigit() or char == "_" for char in op
        ):
            safe["op"] = op
        if "session_id" in event:
            safe["session_match"] = event["session_id"] == session_id
        if safe:
            safe_lines.append(json.dumps(safe, sort_keys=True))
    return "\n".join(safe_lines) or "<no complete lifecycle events in final 8 KiB>"


def ctrl_c_metadata_diagnostics(state_dir: Path, session_id: str, returncode: int | None) -> str:
    """Describe a missing post-Created snapshot without waiting or changing state."""
    snapshot = state_dir / "sessions" / f"{session_id}.json"
    paths = {
        "snapshot": snapshot,
        "snapshot temp": snapshot.with_suffix(".tmp"),
        "snapshot tombstone": snapshot.with_suffix(".json.tombstone"),
        "worker log": state_dir / "logs" / f"{session_id}.log",
    }
    details = [f"client returncode={returncode}"]
    for label, path in paths.items():
        details.append(f"{label}={path} exists={path.is_file()}")

    daemon_info = state_dir / "daemon.json"
    if daemon_info.is_file():
        try:
            pid = int(json.loads(daemon_info.read_text(encoding="utf-8"))["pid"])
            details.append(f"daemon pid={pid} alive={pid_is_alive(pid)}")
        except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
            details.append(f"daemon identity unreadable: {error!r}")
    else:
        details.append("daemon identity missing")

    events = state_dir / "daemon-events.jsonl"
    if events.is_file():
        try:
            details.append("daemon event tail:\n" + safe_daemon_event_tail(events, session_id))
        except OSError as error:
            details.append(f"daemon event tail unreadable: {error!r}")
    else:
        details.append("daemon event tail missing")
    return "\n".join(details)


def test_wait_for_ctrl_c_profile_treats_retired_session_as_retryable() -> None:
    def retired_session() -> dict:
        raise FileNotFoundError("session metadata retired")

    try:
        assert wait_for_ctrl_c_profile(retired_session, timeout=0.05) is None
    except FileNotFoundError:
        pytest.fail("retired session metadata must make the outer attempt retry")


def test_wait_for_ctrl_c_profile_retries_missing_metadata_then_reads_profile() -> None:
    calls = 0

    def delayed_metadata() -> dict:
        nonlocal calls
        calls += 1
        if calls == 1:
            raise FileNotFoundError("snapshot replacement in progress")
        return {"exit_code": 130, "ctrl_c": {"daemon_kill_ms": 12}}

    result = wait_for_ctrl_c_profile(delayed_metadata, timeout=0.5)
    assert result is not None
    assert result[1]["daemon_kill_ms"] == 12
    assert calls == 2


def test_wait_for_ctrl_c_profile_respects_one_deadline_for_absent_metadata() -> None:
    started = time.monotonic()
    result = wait_for_ctrl_c_profile(
        lambda: (_ for _ in ()).throw(FileNotFoundError("snapshot absent")),
        timeout=0.05,
    )
    assert result is None
    assert time.monotonic() - started < 0.25


def test_ctrl_c_metadata_diagnostics_identifies_retired_snapshot(tmp_path: Path) -> None:
    state_dir = tmp_path / "daemon-state"
    sessions = state_dir / "sessions"
    sessions.mkdir(parents=True)
    (sessions / "sess-1.json.tombstone").write_text("{}", encoding="utf-8")

    details = ctrl_c_metadata_diagnostics(state_dir, "sess-1", 130)
    assert "client returncode=130" in details
    assert "sess-1.json exists=False" in details
    assert "sess-1.json.tombstone exists=True" in details
    assert "daemon identity missing" in details


def test_ctrl_c_metadata_diagnostics_bounds_and_filters_event_tail(tmp_path: Path) -> None:
    state_dir = tmp_path / "daemon-state"
    state_dir.mkdir()
    secret = "sensitive-provider-key"
    events = state_dir / "daemon-events.jsonl"
    events.write_text(
        json.dumps({"op": "oversized", "reason": secret * 1000}) + "\n"
        + json.dumps({"ts_ms": 123, "event_id": 4, "daemon_pid": 5,
                      "op": "ctrl_c_kill", "reason": secret, "session_id": "sess-1"}) + "\n",
        encoding="utf-8",
    )

    details = ctrl_c_metadata_diagnostics(state_dir, "sess-1", 130)
    assert "ctrl_c_kill" in details
    assert "session_match" in details
    assert secret not in details
    assert len(details) < 2000


def test_kill_daemon_for_session_treats_an_already_stopped_shared_daemon_as_clean(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A second session cleanup can race a first cleanup of their daemon."""
    state_dir = tmp_path / "daemon-state"
    session_id = "shared-daemon-session"
    metadata_path = state_dir / "sessions" / f"{session_id}.json"
    metadata_path.parent.mkdir(parents=True)
    metadata_path.write_text("{}", encoding="utf-8")

    def shared_daemon_metadata(_state_dir: Path, _session_id: str) -> dict[str, int]:
        return {"daemon_pid": 12345}

    def daemon_was_already_stopped(_pid: int) -> None:
        raise ProcessLookupError("shared daemon already stopped")

    monkeypatch.setattr(_daemon_helpers, "session_metadata", shared_daemon_metadata)
    monkeypatch.setattr(_daemon_helpers, "kill_process", daemon_was_already_stopped)

    _daemon_helpers.kill_daemon_for_session(state_dir, session_id)


class TestDaemonManagedSessionFlags:
    def test_transcript_cross_route_keeps_bridge_in_worker_and_out_of_metadata(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        transcript = tmp_path / "session.transcript"
        agent_report = tmp_path / "agent-report.json"
        env = managed_env(mock_env, state_dir)
        env["ANTHROPIC_API_KEY"] = "ambient-secret-that-must-not-leak"

        result = process.run(
            [
                str(clud_binary),
                "--transcript",
                str(transcript),
                "--codex",
                "--harness",
                "claude",
                "--subprocess",
                "-p",
                "cross-route-daemon",
                "--",
                "--mock-report-file",
                str(agent_report),
                "--mock-sleep-ms",
                "500",
            ],
            capture_output=True,
            text=True,
            timeout=15,
            env=env,
        )
        assert result.returncode == 0, result.stderr
        session_id = read_session_id_from_text(result.stderr)
        try:
            report = json.loads(agent_report.read_text(encoding="utf-8"))
            assert "claude" in report["program"].lower()
            assert report["env"]["ANTHROPIC_BASE_URL_PRESENT"] is True
            assert report["env"]["ANTHROPIC_AUTH_TOKEN_PRESENT"] is True
            assert report["env"]["ANTHROPIC_API_KEY_PRESENT"] is False
            metadata = json.dumps(session_metadata(state_dir, session_id))
            assert "ANTHROPIC_AUTH_TOKEN" not in metadata
            assert "ambient-secret-that-must-not-leak" not in metadata
        finally:
            kill_daemon_for_session(state_dir, session_id)

    def test_transcript_flag_forces_daemon_and_writes_output(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        transcript = tmp_path / "session.transcript"
        env = managed_env(mock_env, state_dir)

        result = process.run(
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

            report = attach_for_report(
                clud_binary, env, state_dir, session_id, "hello-detach"
            )
            assert "hello-detach" in report["args"]
        finally:
            kill_daemon_for_session(state_dir, session_id)

    def test_attach_without_session_id_lists_attachable_sessions(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        state_dir = tmp_path / "daemon-state"
        env = managed_env(mock_env, state_dir)
        launch_cwd = tmp_path / "workspace"
        launch_cwd_2 = tmp_path / "workspace-2"
        launch_cwd.mkdir()
        launch_cwd_2.mkdir()
        # Create two sessions so attach (no args) lists instead of auto-attaching
        proc1, session_id = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "list-attachable",
            "--",
            "--mock-sleep-ms",
            "60000",
            cwd=launch_cwd,
        )
        proc2, session_id_2 = launch_detached(
            clud_binary,
            env,
            "--codex",
            "-p",
            "list-attachable-2",
            "--",
            "--mock-sleep-ms",
            "60000",
            cwd=launch_cwd_2,
        )
        try:
            assert wait_for_exit(proc1, timeout=DETACH_EXIT_TIMEOUT) == 0
            assert wait_for_exit(proc2, timeout=DETACH_EXIT_TIMEOUT) == 0

            listed = process.run(
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
            kill_daemon_for_session(state_dir, session_id_2)

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

            listed = process.run(
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
            kwargs["creationflags"] = process.CREATE_NEW_PROCESS_GROUP

        proc = process.Popen(
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
            stdout=process.PIPE,
            stderr=process.PIPE,
            stdin=process.PIPE,
            text=True,
            env=env,
            **kwargs,
        )

        try:
            session_id = read_session_id(proc)
            listed = process.run(
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
        listed = process.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert listed.returncode == 0
        assert session_id in listed.stdout

        try:
            report = attach_for_report(
                clud_binary, env, state_dir, session_id, "hello-detachable"
            )
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
            kwargs["creationflags"] = process.CREATE_NEW_PROCESS_GROUP

        proc = process.Popen(
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
            stdout=process.PIPE,
            stderr=process.PIPE,
            stdin=process.PIPE,
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
        listed = process.run(
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
                kwargs["creationflags"] = process.CREATE_NEW_PROCESS_GROUP

            proc = process.Popen(
                [
                    str(clud_binary),
                    "--codex",
                    "-p",
                    "fast-ctrl-c",
                    "--",
                    "--mock-sleep-ms",
                    "30000",
                ],
                stdout=process.PIPE,
                stderr=process.PIPE,
                stdin=process.PIPE,
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

                result = wait_for_ctrl_c_profile(
                    lambda state_dir=state_dir, session_id=session_id: read_ctrl_c_metadata_once(
                        state_dir, session_id
                    )
                )
                if result is None:
                    errors.append(
                        f"attempt {attempt}: Ctrl-C profile incomplete after 10s\n"
                        + ctrl_c_metadata_diagnostics(state_dir, session_id, proc.returncode)
                    )
                    continue
                _metadata, profile = result

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
        # #333: exercise the Windows runtime-cache relay while this test's
        # captured pipes and detached daemon make handle inheritance observable.
        # Without the relay's explicit non-inheritable stdio copies,
        # process.run() never receives EOF after the background daemon starts.
        env["CLUD_USE_RUNTIME_CACHE"] = "1"

        result = process.run(
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

        listed = process.run(
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

        first_attach = process.Popen(
            [str(clud_binary), "attach", session_id],
            stdout=process.PIPE,
            stderr=process.PIPE,
            text=True,
            env=env,
        )

        try:
            wait_for_exit(proc, timeout=10)
            time.sleep(1.0)
            assert first_attach.poll() is None

            second_attach = process.run(
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
            kwargs["creationflags"] = process.CREATE_NEW_PROCESS_GROUP

        proc = process.Popen(
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
            stdout=process.PIPE,
            stderr=process.PIPE,
            stdin=process.PIPE,
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

        listed = process.run(
            [str(clud_binary), "list"],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert listed.returncode == 0
        assert session_id in listed.stdout

        kill_daemon_for_session(state_dir, session_id)
