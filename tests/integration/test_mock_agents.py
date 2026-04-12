"""Integration tests: verify clud correctly invokes mock claude/codex agents."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.integration


def _run(
    clud: Path,
    *args: str,
    env: dict[str, str],
    input_data: str | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(clud), *args],
        capture_output=True,
        text=True,
        timeout=15,
        env=env,
        input=input_data,
    )


def _parse_agent_report(result: subprocess.CompletedProcess[str]) -> dict:
    """Parse the JSON report from the mock agent."""
    return json.loads(result.stdout)


class TestBackendSelection:
    """Verify clud selects the correct backend."""

    def test_default_is_claude(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "claude" in report["program"].lower()

    def test_codex_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "--codex", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "codex" in report["program"].lower()

    def test_claude_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "--claude", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "claude" in report["program"].lower()


class TestYoloMode:
    """Verify YOLO mode (--dangerously-skip-permissions) injection."""

    def test_yolo_injected_by_default(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        result = _run(clud_binary, "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "--dangerously-skip-permissions" in report["args"]

    def test_safe_mode_no_yolo(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "--safe", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "--dangerously-skip-permissions" not in report["args"]


class TestPromptDelivery:
    """Verify prompts are correctly delivered to the backend."""

    def test_prompt_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-p", "hello world", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "-p" in report["args"]
        assert "hello world" in report["args"]

    def test_message_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-m", "fix the bug", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "-m" in report["args"]
        assert "fix the bug" in report["args"]


class TestSessionManagement:
    """Verify continue/resume flags are forwarded."""

    def test_continue_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-c", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "--continue" in report["args"]


class TestModelSelection:
    """Verify model preference is forwarded."""

    def test_model_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "--model", "opus", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "--model" in report["args"]
        assert "opus" in report["args"]


class TestExitCodePropagation:
    """Verify clud propagates the backend's exit code."""

    def test_exit_code_zero(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-p", "hello", env=mock_env)
        assert result.returncode == 0

    def test_exit_code_nonzero(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        # The mock agent supports --mock-exit-code but clud forwards unknown flags.
        # We need to pass it via -- separator so clud forwards it.
        result = _run(clud_binary, "-p", "hello", "--", "--mock-exit-code", "42", env=mock_env)
        assert result.returncode == 42


class TestCommandPrompts:
    """Verify special commands inject the expected prompt text."""

    def test_up_command(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "up", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "-p" in report["args"]
        # Find the prompt text (the arg after -p)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert "lint" in prompt.lower()
        assert "commit" in prompt.lower()

    def test_rebase_command(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "rebase", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert "rebase" in prompt.lower()

    def test_fix_command(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "fix", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert "fix" in prompt.lower() or "lint" in prompt.lower()


class TestFlagForwarding:
    """Verify unknown flags are forwarded to the backend."""

    def test_unknown_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(
            clud_binary, "--some-unknown-flag", "-p", "hello", env=mock_env
        )
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "--some-unknown-flag" in report["args"]

    def test_passthrough_after_separator(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        result = _run(
            clud_binary, "-p", "hello", "--", "--verbose", "--debug", env=mock_env
        )
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "--verbose" in report["args"]
        assert "--debug" in report["args"]


class TestPipeMode:
    """Verify piped stdin is delivered to the backend."""

    def test_pipe_input(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, env=mock_env, input_data="piped prompt\n")
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert "-p" in report["args"]
        assert "piped prompt" in report["args"]


class TestLoopMode:
    """Verify loop mode runs multiple iterations."""

    def test_loop_iterations(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(
            clud_binary, "loop", "--loop-count", "3", "do stuff", env=mock_env
        )
        assert result.returncode == 0
        # The mock agent is invoked 3 times, each prints a JSON line.
        lines = [
            line for line in result.stdout.strip().splitlines() if line.strip()
        ]
        assert len(lines) == 3
        for line in lines:
            report = json.loads(line)
            assert "do stuff" in report["args"]
