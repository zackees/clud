"""Executable wait recovery through the real hook, without remote reads (#2004)."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from tests import process

TOOLS = [("Bash", "command"), ("exec_command", "cmd"),
         ("functions.exec_command", "cmd"), ("shell_command", "command")]
WAITER = ("clud tool run github/pr_merge_watch.py -- 123 --repo zackees/clud "
          "--timeout 540 --require '.*' --no-cancel")


def scan(tmp_path: Path, tool: str, key: str, command: str) -> process.CompletedProcess[str]:
    binary = os.environ["CLUD_TEST_BINARY"]
    settings = tmp_path / ".clud" / "settings.json"
    settings.parent.mkdir(exist_ok=True)
    settings.write_text(json.dumps({"git": {"pr_wait_fail_fast": True}}), encoding="utf-8")
    env = {name: value for name, value in os.environ.items() if not name.startswith("CLUD_")}
    env.update(HOME=str(tmp_path), USERPROFILE=str(tmp_path), CLUD_HOOK_HOME=str(tmp_path),
               CLUD_SKIP_RM_IDENTITY="1", CLUD_NO_UNLOCK="1", CLUD_SERVER_SETTINGS="0")
    return process.run(
        [binary, "__cmd-scan", "--event", "PreToolUse"],
        input=json.dumps({"hook_event_name": "PreToolUse", "tool_name": tool,
                          "tool_input": {key: command}, "cwd": str(tmp_path)}),
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=20,
    )


@pytest.mark.parametrize(("tool", "key"), TOOLS)
def test_rejected_watch_offers_verified_execution_capabilities(
    tmp_path: Path, tool: str, key: str
) -> None:
    result = scan(tmp_path, tool, key, "gh run watch 123 --exit-status")
    assert result.returncode == 2, result.stdout + result.stderr
    reason = json.loads(result.stdout)["hookSpecificOutput"]["permissionDecisionReason"]
    for required in ["exec_command", "write_stdin", "empty input", "same session",
                     "run_in_background", "completion notification", "bounded foreground",
                     "capabilities", "tool name", "local process", "Never poll",
                     "pr_merge_watch.py -- <PR>"]:
        assert required in reason, f"missing {required!r}: {reason}"


@pytest.mark.parametrize(("tool", "key"), TOOLS)
def test_canonical_bounded_waiter_remains_allowed(tmp_path: Path, tool: str, key: str) -> None:
    result = scan(tmp_path, tool, key, WAITER)
    assert result.returncode == 0, result.stdout + result.stderr


@pytest.mark.parametrize(("tool", "key"), TOOLS)
def test_status_polling_cannot_replace_session_waiting(tmp_path: Path, tool: str, key: str) -> None:
    result = scan(tmp_path, tool, key, "gh pr checks 123; sleep 5")
    assert result.returncode == 2, result.stdout + result.stderr
    assert "poll" in result.stderr.lower()


HARNESS = Path(__file__).resolve().parent / "fixtures" / "gh_watch_harness.py"


def watcher_env(tmp_path: Path, scenario: str, *, controlled: bool = True) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items() if not key.startswith("CLUD_")}
    env.update(HOME=str(tmp_path), USERPROFILE=str(tmp_path),
               GH_WATCH_SCENARIO=scenario, GH_WATCH_CANCEL_LOG=str(tmp_path / "cancels"),
               GH_WATCH_CLUD_BINARY=os.environ["CLUD_TEST_BINARY"])
    if controlled:
        env["GH_WATCH_CONTROLLED_SESSION"] = "1"
    return env


@pytest.mark.parametrize(("scenario", "extra", "expected"), [
    ("green", [], 0), ("fail", [], 1), ("review", [], 2),
    ("green", ["--not-a-real-flag"], 64), ("timeout", [], 4),
    ("scope", [], 0), ("scope", ["--require", ".*"], 4),
    pytest.param("watchdog", [], 124, marks=pytest.mark.skipif(
        sys.platform == "win32", reason="the local blocked uv fixture uses /bin/sh")),
])
def test_recovery_keeps_one_live_process_until_its_real_terminal_result(
    tmp_path: Path, scenario: str, extra: list[str], expected: int
) -> None:
    """Controlled execution/session contract, not a real Codex runtime integration.

    The child runs the production waiter with offline GitHub snapshots. Its
    stdin handshake makes the observation deadline deterministic: only the
    fixture controller releases it; the modeled session wait sends no input.
    """
    denied = scan(tmp_path, "Bash", "command", "gh run watch 123 --exit-status")
    assert denied.returncode == 2, denied.stderr
    reason = denied.stdout + denied.stderr
    assert "exec_command" in reason and "write_stdin" in reason
    assert scan(tmp_path, "Bash", "command", WAITER).returncode == 0
    argv = [sys.executable, str(HARNESS), "--", "123", "--repo", "zackees/clud",
            "--timeout", "540", "--no-cancel", *extra]
    child = process.Popen(argv, env=watcher_env(tmp_path, scenario), cwd=tmp_path,
                          stdin=process.PIPE, stdout=process.PIPE, stderr=process.PIPE, text=True)
    session_id = child.pid
    try:
        assert str(child.next_stdout(timeout=10)).strip() == "CONTROLLED_SESSION_READY"
        # An empty-input, bounded wait observes the same still-live process.
        with pytest.raises(process.TimeoutExpired):
            child.wait(timeout=0.01)
        assert child.pid == session_id and child.poll() is None
        marker = tmp_path / "already-authorized-next-action"
        assert not marker.exists()
        # Fixture control only; this is not input sent by write_stdin's empty wait.
        assert child.stdin is not None
        child.stdin.write("continue\n")
        child.stdin.flush()
        code = child.wait(timeout=15)
        assert child.pid == session_id
        assert code == expected, (child.stdout.read(), child.stderr.read())
        if code == 0:
            result = process.run(
                [sys.executable, "-c", "from pathlib import Path; import sys; "
                 "Path(sys.argv[1]).write_text('continued')", str(marker)],
                capture_output=True, text=True, timeout=5,
            )
            assert result.returncode == 0, result.stderr
            assert marker.read_text() == "continued"
        else:
            assert not marker.exists()
            if code == 124:
                stderr = child.stderr.read()
                assert '"status":"in-progress"' in stderr or '"status": "in-progress"' in stderr
                assert "resume_hint" in stderr
        assert not (tmp_path / "cancels").exists()
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=5)


def test_require_help_describes_precedence_and_all_check_scope(tmp_path: Path) -> None:
    result = process.run([sys.executable, str(HARNESS), "--help"],
                         env=watcher_env(tmp_path, "green", controlled=False), cwd=tmp_path,
                         capture_output=True, text=True, timeout=10)
    assert result.returncode == 0, result.stderr
    assert "overrides branch protection" in result.stdout
    assert "all reported checks" in result.stdout
