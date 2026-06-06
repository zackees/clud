"""Issue #260: surface-level subprocess tests for `clud hook <verb>`.

The four `clud hook` subcommands are short-lived processes invoked by
Claude Code / Codex at session lifecycle events. Each hook reads a JSON
payload from stdin, talks HTTP to the daemon's `/memory/*` routes, and
exits 0 unconditionally. These tests cover the argv parser + stdin
roundtrip + exit-code contract; the in-process recall + save logic is
covered by the Rust unit tests under
`crates/clud-bin/src/hooks_tests.rs`.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _cargo_argv(subcommand: list[str]) -> list[str]:
    if sys.platform == "win32":
        try:
            from ci.env import build_env, cargo_argv

            return cargo_argv(subcommand, env=build_env())
        except Exception:
            pass
        soldr = shutil.which("soldr")
        if soldr:
            return [soldr, "cargo", *subcommand]
    return ["cargo", *subcommand]


def _clud_binary() -> str:
    env_binary = os.environ.get("CLUD_TEST_BINARY")
    if env_binary and Path(env_binary).is_file():
        return env_binary

    import json as _json

    result = subprocess.run(
        _cargo_argv(["build", "-p", "clud", "--no-default-features", "--message-format=json"]),
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to build clud:\n{result.stderr}")

    for line in result.stdout.splitlines():
        try:
            msg = _json.loads(line)
        except _json.JSONDecodeError:
            continue
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == "clud"
            and msg.get("executable")
        ):
            return msg["executable"]

    ext = ".exe" if sys.platform == "win32" else ""
    for fallback in (
        ROOT / "target" / "x86_64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "aarch64-pc-windows-msvc" / "debug" / f"clud{ext}",
        ROOT / "target" / "debug" / f"clud{ext}",
    ):
        if fallback.is_file():
            return str(fallback)
    raise RuntimeError("clud binary not found after build")


CLUD = _clud_binary()


def _run(
    *args: str,
    stdin: str | None = None,
    env_overrides: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run clud in a tempdir with copied binary; mirror tests/test_memory_cli.py."""
    with tempfile.TemporaryDirectory() as temp_dir:
        source = Path(CLUD)
        launch = Path(temp_dir) / source.name
        shutil.copy2(source, launch)
        env = os.environ.copy()
        if env_overrides:
            env.update(env_overrides)
        return subprocess.run(
            [str(launch), *args],
            capture_output=True,
            text=True,
            input=stdin,
            timeout=20,
            env=env,
        )


def test_hook_help_lists_all_four_verbs() -> None:
    result = _run("hook", "--help")
    assert result.returncode == 0, result.stderr
    out = result.stdout + result.stderr
    for verb in ("session-start", "user-prompt-submit", "post-tool-use", "stop"):
        assert verb in out, f"missing {verb!r} in hook --help:\n{out}"


def test_hook_session_start_emits_context_block() -> None:
    # No daemon required: hook handlers exit 0 silently when the daemon
    # is unreachable, but they still emit a well-formed `<context>`
    # block so the upstream injection site does not have to special-case
    # a missing block.
    result = _run(
        "hook", "session-start", stdin='{"session_id":"test-sid","cwd":"/tmp"}'
    )
    assert result.returncode == 0, result.stderr
    assert "<context source=\"clud-memory\">" in result.stdout, result.stdout
    assert "</context>" in result.stdout, result.stdout


def test_hook_session_start_handles_empty_stdin() -> None:
    result = _run("hook", "session-start", stdin="")
    assert result.returncode == 0, result.stderr
    assert "<context source=\"clud-memory\">" in result.stdout


def test_hook_session_start_handles_malformed_json() -> None:
    result = _run("hook", "session-start", stdin="this is not json")
    assert result.returncode == 0, result.stderr
    # Even malformed input must produce a parseable `<context>` block.
    assert "<context source=\"clud-memory\">" in result.stdout


def test_hook_user_prompt_submit_stdin_roundtrip_no_directive() -> None:
    payload = '{"session_id":"s1","prompt":"can you help me debug this"}'
    result = _run("hook", "user-prompt-submit", stdin=payload)
    # No directive in the prompt: no save attempted, no stdout, exit 0.
    assert result.returncode == 0, result.stderr
    assert result.stdout == "", f"expected empty stdout, got {result.stdout!r}"


def test_hook_user_prompt_submit_exits_zero_on_malformed_payload() -> None:
    result = _run("hook", "user-prompt-submit", stdin="garbage payload")
    assert result.returncode == 0, result.stderr


def test_hook_post_tool_use_is_silent_noop() -> None:
    payload = '{"session_id":"s1","tool_name":"Read","tool_input":{"path":"a"}}'
    result = _run("hook", "post-tool-use", stdin=payload)
    assert result.returncode == 0, result.stderr
    assert result.stdout == "", f"post-tool-use must not pollute stdout: {result.stdout!r}"


def test_hook_stop_exits_zero_without_consolidate_env() -> None:
    payload = '{"session_id":"s1","reason":"user_quit"}'
    result = _run("hook", "stop", stdin=payload)
    assert result.returncode == 0, result.stderr
    assert result.stdout == "", f"stop must not pollute stdout: {result.stdout!r}"


def test_hook_stop_exits_zero_with_consolidate_env_set() -> None:
    # When the route does not yet exist, the consolidate path falls
    # back to a debug log (silent unless CLUD_MEMORY_DEBUG_HOOKS=1).
    payload = '{"session_id":"s1","reason":"task_done"}'
    result = _run(
        "hook",
        "stop",
        stdin=payload,
        env_overrides={"CLUD_MEMORY_AUTO_CONSOLIDATE_ON_STOP": "1"},
    )
    assert result.returncode == 0, result.stderr


def test_hook_debug_env_surfaces_diagnostic_on_stderr() -> None:
    # With CLUD_MEMORY_DEBUG_HOOKS=1, debug_log writes to stderr. We
    # verify the env-var path by feeding malformed JSON: the parse
    # failure path emits a `[clud-hook]` diagnostic.
    result = _run(
        "hook",
        "stop",
        stdin="not json",
        env_overrides={"CLUD_MEMORY_DEBUG_HOOKS": "1"},
    )
    assert result.returncode == 0, result.stderr
    assert "[clud-hook]" in result.stderr, f"debug log missing: {result.stderr!r}"
