"""Executable wait recovery through the real hook, without remote reads (#2004)."""

from __future__ import annotations

import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
from pathlib import Path

import pytest

from tests import process

TOOLS = [("Bash", "command"), ("exec_command", "cmd"),
         ("functions.exec_command", "cmd"), ("shell_command", "command")]
WAITER = ("clud tool run github/pr_merge_watch.py -- 123 --repo zackees/clud "
          "--timeout 540 --require '.*' --no-cancel")


def scan(
    tmp_path: Path, tool: str, key: str, text: str, **fields: str
) -> process.CompletedProcess[str]:
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
                          "tool_input": {key: text, **fields}, "cwd": str(tmp_path)}),
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


def test_exec_workdir_resolves_the_actual_script_not_a_decoy(tmp_path: Path) -> None:
    nested = tmp_path / "nested"
    nested.mkdir()
    (tmp_path / "watch.sh").write_text("echo harmless\n", encoding="utf-8")
    (nested / "watch.sh").write_text(
        "while gh pr checks 123; do sleep 5; done\n", encoding="utf-8")
    result = scan(tmp_path, "functions.exec_command", "cmd", "bash watch.sh", workdir="nested")
    assert result.returncode == 2, result.stdout + result.stderr
    assert "poll" in result.stderr.lower()


def test_exec_rewrite_updates_cmd_without_changing_unrelated_fields(tmp_path: Path) -> None:
    result = scan(tmp_path, "exec_command", "cmd", "rm note.txt", shell="bash", workdir=".")
    assert result.returncode == 0, result.stderr
    updated = json.loads(result.stdout)["hookSpecificOutput"]["updatedInput"]
    assert "safe-rm" in updated["cmd"]
    assert "command" not in updated
    assert updated["shell"] == "bash" and updated["workdir"] == "."


def test_exec_honors_declared_shell_for_wait_syntax(tmp_path: Path) -> None:
    result = scan(tmp_path, "exec_command", "cmd",
                  "foreach ($i in 1..10) { Write-Output tick; Start-Sleep 5 }", shell="pwsh")
    assert result.returncode == 2, result.stdout + result.stderr
    assert "wait" in result.stderr.lower()


@pytest.mark.parametrize("tool", ["exec_command", "functions.exec_command"])
def test_extra_command_fields_cannot_shadow_the_declared_cmd(tmp_path: Path, tool: str) -> None:
    result = scan(tmp_path, tool, "cmd", "gh run watch 123 --exit-status",
                  command="echo harmless", script="echo also-ignored")
    assert result.returncode == 2, result.stdout + result.stderr
    assert "watch" in result.stderr.lower()


def test_declared_background_completion_waits_for_the_existing_process(tmp_path: Path) -> None:
    """A controlled completion future models an exposed background notification."""
    denied = scan(tmp_path, "Bash", "command", "gh run watch 123")
    assert "run_in_background" in denied.stderr and "completion notification" in denied.stderr
    child = process.Popen(
        [sys.executable, str(HARNESS), "123", "--repo", "zackees/clud", "--no-cancel"],
        env=watcher_env(tmp_path, "green"), cwd=tmp_path,
        stdin=process.PIPE, stdout=process.PIPE, stderr=process.PIPE, text=True,
    )
    with ThreadPoolExecutor(max_workers=1) as executor:
        try:
            assert str(child.next_stdout(timeout=10)).strip() == "CONTROLLED_SESSION_READY"
            completion = executor.submit(child.wait, timeout=15)
            with pytest.raises(FutureTimeout):
                completion.result(timeout=0.01)
            assert child.poll() is None
            assert child.stdin is not None
            child.stdin.write("continue\n")
            child.stdin.flush()
            assert completion.result(timeout=10) == 0
        finally:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)


@pytest.mark.parametrize(("scenario", "code"), [("green", 0), ("timeout", 4)])
def test_bounded_foreground_uses_the_waiters_own_terminal_deadline(
    tmp_path: Path, scenario: str, code: int
) -> None:
    denied = scan(tmp_path, "Bash", "command", "gh run watch 123")
    assert "bounded foreground" in denied.stderr
    result = process.run(
        [sys.executable, str(HARNESS), "--", "123", "--repo", "zackees/clud",
         "--timeout", "540", "--no-cancel"],
        env=watcher_env(tmp_path, scenario, controlled=False), cwd=tmp_path,
        capture_output=True, text=True, timeout=10,
    )
    assert result.returncode == code, result.stderr


def test_ambiguous_tool_names_offer_a_local_capability_probe(tmp_path: Path) -> None:
    denied = scan(tmp_path, "Bash", "command", "gh run watch 123")
    assert "tool name" in denied.stderr and "harmless local process" in denied.stderr
    probe = process.Popen(
        [sys.executable, "-c", "import sys; print('probe-ready', flush=True); sys.stdin.readline()"],
        stdin=process.PIPE, stdout=process.PIPE, stderr=process.PIPE, text=True,
    )
    try:
        assert str(probe.next_stdout(timeout=5)).strip() == "probe-ready"
        pid = probe.pid
        with pytest.raises(process.TimeoutExpired):
            probe.wait(timeout=0.01)
        assert probe.pid == pid and probe.poll() is None
        assert probe.stdin is not None
        probe.stdin.write("complete probe\n")
        probe.stdin.flush()
        assert probe.wait(timeout=5) == 0
    finally:
        if probe.poll() is None:
            probe.kill()
        probe.wait(timeout=5)
