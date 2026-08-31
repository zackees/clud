"""Regression tests for bundled hook stdin handling."""

from __future__ import annotations

import json
import os
import shlex
import shutil
import sys
from pathlib import Path

import pytest

from tests import process

ROOT = Path(__file__).resolve().parent.parent
TELEMETRY = (
    ROOT / "crates" / "clud-bin" / "assets" / "tools" / "hooks" / "telemetry.py"
)
CLAUDE_SOLDR_HOOK = ROOT / ".claude" / "hooks" / "check-soldr.py"
CODEX_SOLDR_HOOK = ROOT / ".codex" / "hooks" / "check-soldr.py"


def _hook_env(home: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["CLUD_HOOK_STDIN_IDLE_TIMEOUT_SEC"] = "0.05"
    env["CLUD_HOOK_STDIN_DEADLINE_SEC"] = "0.25"
    env["CLUD_TELEMETRY_STDIN_IDLE_TIMEOUT_SEC"] = "0.05"
    env["CLUD_TELEMETRY_STDIN_DEADLINE_SEC"] = "0.25"
    return env


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _block_bad_cmd_binary() -> Path:
    env_binary = os.environ.get("CLUD_TEST_BLOCK_BAD_CMD_BINARY")
    if env_binary and Path(env_binary).is_file():
        return Path(env_binary)

    clud_binary = os.environ.get("CLUD_TEST_BINARY")
    if clud_binary:
        sibling = Path(clud_binary).with_name(_binary_name("clud-block-bad-cmd"))
        if sibling.is_file():
            return sibling

    resolved = shutil.which(_binary_name("clud-block-bad-cmd"))
    if resolved:
        return Path(resolved)

    raise AssertionError("clud-block-bad-cmd test binary not found")


def _run_hook_with_open_stdin(
    tmp_path: Path,
    payload: str | None,
    argv: list[str] | None = None,
    extra_env: dict[str, str] | None = None,
) -> process.CompletedProcess[str]:
    env = _hook_env(tmp_path / "home")
    if extra_env:
        env.update(extra_env)
    if argv is None:
        argv = [str(_block_bad_cmd_binary())]
    proc = process.Popen(
        argv,
        stdin=process.PIPE,
        stdout=process.PIPE,
        stderr=process.PIPE,
        text=True,
        env=env,
    )
    assert proc.stdin is not None
    assert proc.stdout is not None
    assert proc.stderr is not None
    if payload is not None:
        proc.stdin.write(payload)
        proc.stdin.flush()

    try:
        returncode = proc.wait(timeout=5.0)
    except process.TimeoutExpired:
        proc.kill()
        stdout, stderr = proc.communicate(timeout=1.0)
        raise AssertionError(
            f"hook did not exit while stdin pipe remained open; stdout={stdout!r} "
            f"stderr={stderr!r}"
        ) from None

    proc.stdin.close()
    stdout = proc.stdout.read()
    stderr = proc.stderr.read()
    return process.CompletedProcess(proc.args, returncode, stdout, stderr)


def test_block_bad_cmd_reads_payload_without_waiting_for_stdin_eof(tmp_path: Path) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "bad" + " cmd"},
        }
    )

    result = _run_hook_with_open_stdin(tmp_path, payload)

    assert result.returncode == 2
    assert "permissionDecision" in result.stdout
    assert "deny" in result.stdout
    assert "refusing to run" in result.stderr


def test_block_bad_cmd_allows_missing_payload_without_waiting_for_stdin_eof(
    tmp_path: Path,
) -> None:
    result = _run_hook_with_open_stdin(tmp_path, None)

    assert result.returncode == 0
    log_path = tmp_path / "home" / ".clud" / "tools" / "hooks" / "block-bad-cmd.log"
    log = log_path.read_text(encoding="utf-8")
    assert "stdin_read_incomplete" in log
    assert "raw_stdin_bytes=0" in log


def test_block_bad_cmd_allows_malformed_json(tmp_path: Path) -> None:
    result = _run_hook_with_open_stdin(tmp_path, "{not-json")

    assert result.returncode == 0
    assert "permissionDecision" not in result.stdout


# --- #1064: unverifiable payloads fail closed for removals only -------------
#
# These feed the hook payloads it cannot parse. Nothing here ever executes a
# command: the hook reads stdin and prints a decision, so the `rm -rf` text
# below is inert data used to steer that decision.


def test_unparseable_payload_naming_a_removal_is_denied(tmp_path: Path) -> None:
    """A removal the hook could not inspect must not be allowed through.

    The payload is cut off mid-JSON and stdin is left open, which is the exact
    shape that used to reach an unconditional `return 0`.
    """
    truncated = '{"tool_name":"Bash","tool_input":{"command":"rm -rf \\"$SP\\"/'

    result = _run_hook_with_open_stdin(tmp_path, truncated)

    assert result.returncode == 2, result.stdout
    hook_output = json.loads(result.stdout)["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "deny"
    assert "removal" in hook_output["permissionDecisionReason"]


def test_unparseable_payload_naming_a_removal_after_a_newline_is_denied(
    tmp_path: Path,
) -> None:
    """A newline inside the command is `\\n` in the payload, not whitespace.

    A probe that demanded real whitespace before `rm` missed every removal
    that began a line, which is the ordinary shape of a multi-line command.
    """
    truncated = '{"tool_name":"Bash","tool_input":{"command":"cd /tmp\\nrm -rf $SP/'

    result = _run_hook_with_open_stdin(tmp_path, truncated)

    assert result.returncode == 2, result.stdout
    assert '"deny"' in result.stdout


@pytest.mark.skipif(
    sys.platform == "win32", reason="POSIX shell needed to hold the pipe open"
)
def test_complete_payload_is_verified_even_when_stdin_never_reaches_eof(
    tmp_path: Path,
) -> None:
    """A held-open pipe is not a truncated payload.

    Claude Code routinely writes a complete payload and leaves stdin open
    (anthropics/claude-code#53177, `windows-quirks.md`), which is the whole
    reason the idle timeout exists. Treating that as unverifiable denied every
    tool call whose text merely mentioned `rm` — including #963's own safe
    rewrite — with retry advice that could never succeed.

    `tests.process` closes the child's stdin on write, so the pipe is held
    open by a shell instead.
    """
    home = tmp_path / "home"
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": 'SP="/tmp/safe/path"; rm -f "$SP"/*.txt'},
            "cwd": str(tmp_path),
        }
    )
    script = (
        f"{{ printf %s {shlex.quote(payload)}; sleep 2; }} "
        f"| {shlex.quote(str(_block_bad_cmd_binary()))}"
    )

    result = process.run(
        ["bash", "-c", script],
        stdout=process.PIPE,
        stderr=process.PIPE,
        text=True,
        env=_hook_env(home),
        timeout=30,
    )

    assert result.returncode == 0, (
        "a complete payload whose writer held the pipe open must still be "
        f"verified normally; stdout={result.stdout!r}"
    )
    # #963 proves this removal safe and rewrites it, rather than denying.
    hook_output = json.loads(result.stdout)["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "allow"
    assert "$SP" not in hook_output["updatedInput"]["command"]

    log = (home / ".clud" / "tools" / "hooks" / "block-bad-cmd.log").read_text(
        encoding="utf-8"
    )
    assert "stdin_read_incomplete" in log, (
        "the read must actually have stopped short, or this test is not "
        "exercising the path it claims to"
    )


def test_unparseable_payload_without_a_removal_is_still_allowed(
    tmp_path: Path,
) -> None:
    """The anti-wedge property: a broken payload is not a reason to block.

    A regression here is worse than the bug #1064 fixed, because it would wall
    off every tool call whenever the hook hiccups.
    """
    for truncated in (
        '{"tool_name":"Bash","tool_input":{"command":"cargo build --rel',
        '{"tool_name":"Bash","tool_input":{"command":"docker run --rm ubuntu',
        "{not-json but mentions armv7 and form/",
    ):
        result = _run_hook_with_open_stdin(tmp_path, truncated)

        assert result.returncode == 0, f"{truncated!r} -> {result.stdout!r}"
        assert "permissionDecision" not in result.stdout


def test_rm_literal_assignment_rewrites_before_backend_prompt(tmp_path: Path) -> None:
    """#963: resolve a preceding literal assignment before Claude can ask."""
    scratchpad = "C:/Users/test/.clud/tmp/claude/session/scratchpad"
    command = (
        'git status --porcelain; '
        f'SP="{scratchpad}"; '
        'rm -f "$SP"/*.txt "$SP"/*.json "$SP"/*.md 2>/dev/null; '
        'ls "$SP"'
    )
    tool_input = {
        "command": command,
        "description": "clear scratchpad",
        "timeout": 120_000,
        "run_in_background": False,
        "future_field": {"preserve": True},
    }
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": tool_input,
            "cwd": str(tmp_path),
        }
    )

    result = _run_hook_with_open_stdin(tmp_path, payload)

    assert result.returncode == 0, result.stderr
    output = json.loads(result.stdout)
    hook_output = output["hookSpecificOutput"]
    assert hook_output["hookEventName"] == "PreToolUse"
    assert hook_output["permissionDecision"] == "allow"
    updated = hook_output["updatedInput"]
    assert updated.keys() == tool_input.keys()
    assert updated["description"] == "clear scratchpad"
    assert updated["timeout"] == 120_000
    assert updated["run_in_background"] is False
    assert updated["future_field"] == {"preserve": True}
    assert "$SP" not in updated["command"].split("rm -f", 1)[1].split(";", 1)[0]
    assert updated["command"].count(scratchpad) == 4


def test_rm_unresolved_variable_is_a_structured_noninteractive_denial(
    tmp_path: Path,
) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": 'rm -rf "$UNSET"/*'},
            "cwd": str(tmp_path),
        }
    )

    result = _run_hook_with_open_stdin(tmp_path, payload)

    assert result.returncode == 2
    hook_output = json.loads(result.stdout)["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "deny"
    assert "could not be proven" in hook_output["permissionDecisionReason"]
    assert "ask" not in result.stdout


def test_rm_rewrite_and_deny_support_camel_case_codex_payloads(
    tmp_path: Path,
) -> None:
    tool_input = {
        "command": 'SP=/tmp/safe/path; rm -f "$SP"/*.txt',
        "timeoutMs": 30_000,
        "futureField": {"preserve": True},
    }
    rewrite_payload = json.dumps(
        {
            "toolName": "Bash",
            "toolInput": tool_input,
            "cwdPath": str(tmp_path),
        }
    )

    rewritten = _run_hook_with_open_stdin(tmp_path, rewrite_payload)

    assert rewritten.returncode == 0, rewritten.stderr
    hook_output = json.loads(rewritten.stdout)["hookSpecificOutput"]
    assert hook_output["permissionDecision"] == "allow"
    assert hook_output["updatedInput"]["timeoutMs"] == 30_000
    assert hook_output["updatedInput"]["futureField"] == {"preserve": True}
    assert "$SP" not in hook_output["updatedInput"]["command"].split("rm -f", 1)[1]

    deny_payload = json.dumps(
        {
            "toolName": "Bash",
            "toolInput": {"command": 'rm -rf "$UNSET"/*'},
            "cwdPath": str(tmp_path),
        }
    )
    denied = _run_hook_with_open_stdin(tmp_path, deny_payload)
    assert denied.returncode == 2
    denied_output = json.loads(denied.stdout)["hookSpecificOutput"]
    assert denied_output["permissionDecision"] == "deny"
    assert "ask" not in denied.stdout


def test_normal_command_remains_silent_instead_of_emitting_bare_allow(
    tmp_path: Path,
) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "echo ordinary"},
            "cwd": str(tmp_path),
        }
    )

    result = _run_hook_with_open_stdin(tmp_path, payload)

    assert result.returncode == 0
    assert result.stdout == ""


def test_telemetry_hook_reads_payload_without_waiting_for_stdin_eof(
    tmp_path: Path,
) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi"},
        }
    )

    result = _run_hook_with_open_stdin(
        tmp_path,
        payload,
        argv=[sys.executable, str(TELEMETRY)],
        extra_env={"CLUD_DAEMON_HTTP_SERVER": "not-a-valid-url"},
    )

    assert result.returncode == 0


def test_tracked_soldr_hooks_read_payload_without_waiting_for_stdin_eof(
    tmp_path: Path,
) -> None:
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "echo hi"},
        }
    )

    scripts = [script for script in (CLAUDE_SOLDR_HOOK, CODEX_SOLDR_HOOK) if script.is_file()]
    assert scripts, "at least one tracked soldr hook must exist"
    for script in scripts:
        result = _run_hook_with_open_stdin(
            tmp_path,
            payload,
            argv=[sys.executable, str(script)],
        )
        assert result.returncode == 0, script


def test_bad_command_denial_includes_rule_provenance(tmp_path: Path) -> None:
    """#525: a config `bad_commands` denial cites the matched token, normalized
    program, rule id, and `<file>#/bad_commands/<index>` source in
    `permissionDecisionReason`, and writes a structured `bad_cmd_denied` event
    to the hook log."""
    repo = tmp_path / "repo"
    (repo / ".clud").mkdir(parents=True)
    (repo / ".git").mkdir()
    (repo / ".clud" / "settings.json").write_text(
        json.dumps(
            {
                "bad_commands": [
                    {
                        "id": "manual-check",
                        "match": "clud-manual-bad-command",
                        "replacement": "echo use-the-approved-command",
                        "reason": "manual verification rule triggered",
                    }
                ]
            }
        ),
        encoding="utf-8",
    )
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "clud-manual-bad-command --example"},
            "cwd": str(repo),
        }
    )

    result = _run_hook_with_open_stdin(tmp_path, payload)
    assert result.returncode == 2, result.stderr
    out = result.stdout
    assert "deny" in out
    # Provenance appended to permissionDecisionReason.
    assert "Blocked" in out
    assert "clud-manual-bad-command" in out
    assert "normalized:" in out
    assert "by rule `manual-check`" in out
    assert "#/bad_commands/0`" in out

    # Structured forensic event in the hook log.
    log = tmp_path / "home" / ".clud" / "tools" / "hooks" / "block-bad-cmd.log"
    assert log.is_file(), "hook log should exist"
    log_text = log.read_text(encoding="utf-8")
    assert "bad_cmd_denied" in log_text
    assert "manual-check" in log_text
