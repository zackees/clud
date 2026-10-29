"""Integration tests for Claude Code gateway-discovery version floors (#921).

`clud --unified` advertises connector rows through gateway model discovery,
which Claude Code 2.1.223+ consumes. A pre-floor client must refuse the
launch with a message naming the floor — never a silent launch into a
`/model` picker with no connector rows. The mock `claude` (mock-agent)
reports its version from `MOCK_CLAUDE_VERSION`, so the probe -> refuse path
runs against the real binary end to end.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from tests import process

pytestmark = pytest.mark.integration

_TIMEOUT = 90

_REFUSAL_MARKER = "unified routing requires Claude Code >= 2.1.223"


def _run_unified(
    clud: Path,
    env: dict[str, str],
) -> process.CompletedProcess[str]:
    return process.run(
        [str(clud), "--unified", "-p", "hello"],
        capture_output=True,
        text=True,
        timeout=_TIMEOUT,
        env=env,
    )


def _run_direct_codex(
    clud: Path,
    env: dict[str, str],
) -> process.CompletedProcess[str]:
    return process.run(
        [str(clud), "--codex", "--harness", "claude", "-p", "hello"],
        capture_output=True,
        text=True,
        timeout=_TIMEOUT,
        env=env,
    )


def _assert_rc(result: process.CompletedProcess[str], expected: int) -> None:
    assert result.returncode == expected, (
        f"expected exit {expected}, got {result.returncode}\n"
        f"--- stderr ---\n{result.stderr}\n"
        f"--- stdout (tail) ---\n{result.stdout[-2000:]}"
    )


def test_unified_refuses_a_pre_223_claude_code(
    clud_binary: Path, mock_env: dict[str, str]
) -> None:
    """A pre-floor client refuses, naming the floor, the found version, and the fix."""
    mock_env["MOCK_CLAUDE_VERSION"] = "2.1.212 (Claude Code)"
    result = _run_unified(clud_binary, mock_env)
    _assert_rc(result, 2)
    assert _REFUSAL_MARKER in result.stderr
    assert "installed version is 2.1.212" in result.stderr
    assert "claude update" in result.stderr


def test_unified_launches_at_the_223_floor(
    clud_binary: Path, mock_env: dict[str, str]
) -> None:
    """A client at (or above) the floor launches without the refusal."""
    mock_env["MOCK_CLAUDE_VERSION"] = "2.1.223 (Claude Code)"
    result = _run_unified(clud_binary, mock_env)
    _assert_rc(result, 0)
    assert _REFUSAL_MARKER not in result.stderr


def test_unified_refuses_when_the_version_is_unverifiable(
    clud_binary: Path, mock_env: dict[str, str]
) -> None:
    """Unparseable `--version` output also refuses rather than staying silent."""
    mock_env["MOCK_CLAUDE_VERSION"] = "not a version"
    result = _run_unified(clud_binary, mock_env)
    _assert_rc(result, 2)
    assert "no installed version could be parsed" in result.stderr
    assert "2.1.223" in result.stderr


def test_direct_codex_refuses_a_pre_223_claude_code(
    clud_binary: Path, mock_env: dict[str, str]
) -> None:
    mock_env["MOCK_CLAUDE_VERSION"] = "2.1.212 (Claude Code)"
    result = _run_direct_codex(clud_binary, mock_env)
    _assert_rc(result, 2)
    assert (
        "Codex-through-Claude model discovery requires Claude Code >= 2.1.223"
        in result.stderr
    )
    assert "installed version is 2.1.212" in result.stderr
    assert "claude update" in result.stderr


def test_direct_codex_launches_at_the_223_floor(
    clud_binary: Path, mock_env: dict[str, str]
) -> None:
    mock_env["MOCK_CLAUDE_VERSION"] = "2.1.223 (Claude Code)"
    result = _run_direct_codex(clud_binary, mock_env)
    _assert_rc(result, 0)
