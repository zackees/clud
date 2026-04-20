"""Integration tests: verify clud correctly invokes mock claude/codex agents."""

from __future__ import annotations

import json
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import pytest

pytestmark = pytest.mark.integration

_TIMEOUT = 15
_ANSI_RE = re.compile(r"\x1b(?:\[[^a-zA-Z]*[a-zA-Z]|\][^\x07]*\x07)")


def _strip_ansi(text: str) -> str:
    return _ANSI_RE.sub("", text)


def _run(
    clud: Path,
    *args: str,
    env: dict[str, str],
    input_data: str | None = None,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as temp_dir:
        launch = Path(temp_dir) / clud.name
        shutil.copy2(clud, launch)
        return subprocess.run(
            [str(launch), *args],
            capture_output=True,
            text=True,
            timeout=_TIMEOUT,
            env=env,
            input=input_data,
            cwd=cwd,
        )


def _parse_agent_report(result: subprocess.CompletedProcess[str]) -> dict:
    """Parse the JSON report from the mock agent.

    PTY output may contain ANSI escape sequences around the JSON.
    Extract the first valid JSON object from the cleaned output.
    """
    cleaned = _strip_ansi(result.stdout)
    for line in cleaned.splitlines():
        line = line.strip()
        if line.startswith("{"):
            return json.loads(line)
    return json.loads(cleaned)


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

    def test_codex_preserves_cwd(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "--codex", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert report["cwd"] == str(Path.cwd())

    def test_claude_preserves_cwd(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "--claude", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert report["cwd"] == str(Path.cwd())

    def test_claude_preserves_explicit_launch_cwd(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        result = _run(clud_binary, "--claude", "-p", "hello", env=mock_env, cwd=tmp_path)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert report["cwd"] == str(tmp_path)


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


class TestLaunchMode:
    """Verify launch-mode selection and overrides."""

    def test_dry_run_defaults_to_subprocess(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        result = _run(clud_binary, "--dry-run", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = json.loads(result.stdout)
        assert report["launch_mode"] == "subprocess"

    def test_dry_run_pty_override(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        result = _run(clud_binary, "--dry-run", "--pty", "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = json.loads(result.stdout)
        assert report["launch_mode"] == "pty"


class TestExitCodePropagation:
    """Verify clud propagates the backend's exit code."""

    def test_exit_code_zero(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-p", "hello", env=mock_env)
        assert result.returncode == 0

    def test_exit_code_nonzero(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-p", "hello", "--", "--mock-exit-code", "42", env=mock_env)
        assert result.returncode == 42


class TestCommandPrompts:
    """Verify special commands inject the expected prompt text."""

    def test_up_command(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "up", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert "lint" in prompt.lower()
        assert "codeup" in prompt.lower()
        assert "<your one-line summary>" in prompt

    def test_up_with_message(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "up", "-m", "bump version", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert 'codeup -m "bump version"' in prompt
        assert "<your one-line summary>" not in prompt

    def test_up_with_publish(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "up", "--publish", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert "codeup" in prompt
        assert "-p" in prompt.split("codeup")[1]

    def test_up_with_message_and_publish(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        result = _run(clud_binary, "up", "-m", "release v2", "--publish", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert 'codeup -m "release v2" -p' in prompt

    def test_rebase_command(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "rebase", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert "git fetch" in prompt
        assert "rebase" in prompt.lower()

    def test_fix_command(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "fix", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert "linting" in prompt.lower()
        assert "unit tests" in prompt.lower()

    def test_fix_with_github_url(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        url = "https://github.com/user/repo/actions/runs/123"
        result = _run(clud_binary, "fix", url, env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        idx = report["args"].index("-p")
        prompt = report["args"][idx + 1]
        assert url in prompt
        assert "gh run view" in prompt
        assert "lint-test" in prompt


class TestEnvTracking:
    """Verify clud injects tracking env vars into the child process."""

    def test_in_clud_set(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        assert report["env"]["IN_CLUD"] == "1"

    def test_originator_set(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "-p", "hello", env=mock_env)
        assert result.returncode == 0
        report = _parse_agent_report(result)
        originator = report["env"]["RUNNING_PROCESS_ORIGINATOR"]
        assert originator is not None
        assert originator.startswith("CLUD:")
        pid_str = originator.split(":")[1]
        assert pid_str.isdigit()


class TestFlagForwarding:
    """Verify unknown flags are forwarded to the backend."""

    def test_unknown_flag(self, clud_binary: Path, mock_env: dict[str, str]) -> None:
        result = _run(clud_binary, "--some-unknown-flag", "-p", "hello", env=mock_env)
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
        # --no-done-marker: this test only verifies the loop runs N times;
        # without it, the DONE-marker contract would make exit code 2 (not
        # converged) the expected outcome when no marker is written.
        result = _run(
            clud_binary,
            "loop",
            "--loop-count",
            "3",
            "--no-done-marker",
            "do stuff",
            env=mock_env,
        )
        assert result.returncode == 0
        cleaned = _strip_ansi(result.stdout)
        json_lines = [line.strip() for line in cleaned.splitlines() if line.strip().startswith("{")]
        assert len(json_lines) == 3
        for line in json_lines:
            report = json.loads(line)
            assert "do stuff" in report["args"]

    def test_loop_stops_on_failure(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        result = _run(
            clud_binary,
            "loop",
            "--loop-count",
            "5",
            "task",
            "--",
            "--mock-exit-code",
            "1",
            env=mock_env,
        )
        assert result.returncode == 1
        cleaned = _strip_ansi(result.stdout)
        json_lines = [line.strip() for line in cleaned.splitlines() if line.strip().startswith("{")]
        assert len(json_lines) == 1

    # ---- Issue #48: `clud --codex loop "..."` must work against codex ----

    def test_codex_loop_iterations(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        """`clud --codex loop ...` drives the codex backend for N iterations."""
        result = _run(
            clud_binary,
            "--codex",
            "loop",
            "--loop-count",
            "3",
            "--no-done-marker",
            "do stuff",
            env=mock_env,
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"
        cleaned = _strip_ansi(result.stdout)
        json_lines = [
            line.strip() for line in cleaned.splitlines() if line.strip().startswith("{")
        ]
        assert len(json_lines) == 3
        for line in json_lines:
            report = json.loads(line)
            # Each iteration invoked the codex mock agent.
            assert "codex" in report["program"].lower()
            # Codex uses `exec` subcommand + positional prompt (no `-p` flag).
            assert "exec" in report["args"]
            assert "-p" not in report["args"]
            assert "--dangerously-bypass-approvals-and-sandbox" in report["args"]
            # The prompt appears as a positional arg somewhere in the args.
            assert any("do stuff" in a for a in report["args"])

    def test_codex_loop_dry_run(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        """`clud --codex --dry-run loop ...` emits the codex command."""
        result = _run(
            clud_binary,
            "--codex",
            "--dry-run",
            "loop",
            "task",
            env=mock_env,
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"
        report = json.loads(result.stdout)
        assert report["backend"] == "codex"
        assert report["launch_mode"] == "subprocess"
        assert report["loop_markers"] is not None
        assert report["iterations"] == 50
        cmd = report["command"]
        assert cmd[1] == "exec"
        assert "--dangerously-bypass-approvals-and-sandbox" in cmd
        assert "-p" not in cmd


class TestInterruptReporting:
    """Verify Ctrl+C reports how clud was launched."""

    def test_ctrl_c_reports_pty_mode_when_forced(
        self, clud_binary: Path, mock_env: dict[str, str]
    ) -> None:
        kwargs: dict[str, Any] = {}
        if sys.platform == "win32":
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP

        proc = subprocess.Popen(
            [
                str(clud_binary),
                "--pty",
                "-p",
                "hello",
                "--",
                "--mock-sleep-ms",
                "5000",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=mock_env,
            **kwargs,
        )

        try:
            time.sleep(0.5)
            if sys.platform == "win32":
                proc.send_signal(signal.CTRL_BREAK_EVENT)
            else:
                proc.send_signal(signal.SIGINT)
            _stdout, stderr = proc.communicate(timeout=10)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)

        if sys.platform == "win32":
            assert proc.returncode in (130, 3221225786)
        else:
            assert proc.returncode == 130

        if proc.returncode == 130:
            assert "[clud] interrupted via Ctrl+C (pty)" in stderr


class TestStdinForwarding:
    """Verify stdin data reaches the backend in subprocess mode.

    When clud is launched with an explicit prompt (-p), the pipe-mode stdin
    detection is skipped.  Any data written to clud's stdin should be
    inherited by the child process (subprocess mode uses StdinMode::Inherit).

    The mock agent's --mock-read-stdin-ms flag makes it read from stdin for
    a specified duration and report what it received.
    """

    def _parse_report(
        self,
        report_file: Path,
        stdout: str,
        stderr: str,
    ) -> dict:
        """Parse the mock agent report from file or stdout."""
        if report_file.exists():
            return json.loads(report_file.read_text())
        return _parse_agent_report(
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=stdout, stderr=stderr
            )
        )

    def test_subprocess_stdin_forwarding(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        """Data written to clud's stdin reaches the child in subprocess mode."""
        report_file = tmp_path / "report.json"
        result = _run(
            clud_binary,
            "-p",
            "hello",
            "--",
            "--mock-read-stdin-ms",
            "3000",
            "--mock-report-file",
            str(report_file),
            env=mock_env,
            input_data="dropped-file.txt\n",
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"

        report = self._parse_report(report_file, result.stdout, result.stderr)
        assert report["stdin"] is not None, "mock agent received no stdin data"
        assert "dropped-file.txt" in report["stdin"]

    def test_subprocess_stdin_is_not_terminal_when_piped(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        """When test pipes stdin, the child sees a pipe (not a terminal)."""
        report_file = tmp_path / "report.json"
        result = _run(
            clud_binary,
            "-p",
            "hello",
            "--",
            "--mock-read-stdin-ms",
            "1000",
            "--mock-report-file",
            str(report_file),
            env=mock_env,
            input_data="",
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"

        report = self._parse_report(report_file, result.stdout, result.stderr)
        # With piped stdin, the child should NOT see a terminal
        assert report["stdin_is_terminal"] is False

    def test_subprocess_multiline_stdin(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        """Multi-line paste (simulated drag-and-drop of multiple files) works."""
        report_file = tmp_path / "report.json"
        payload = "file1.txt\nfile2.txt\npath/to/file3.txt\n"

        result = _run(
            clud_binary,
            "-p",
            "hello",
            "--",
            "--mock-read-stdin-ms",
            "3000",
            "--mock-report-file",
            str(report_file),
            env=mock_env,
            input_data=payload,
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"

        report = self._parse_report(report_file, result.stdout, result.stderr)
        assert report["stdin"] is not None
        assert "file1.txt" in report["stdin"]
        assert "file2.txt" in report["stdin"]
        assert "path/to/file3.txt" in report["stdin"]

    def test_subprocess_bracketed_paste_forwarding(
        self, clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
    ) -> None:
        """Bracketed paste sequences are forwarded to the child."""
        report_file = tmp_path / "report.json"
        # Simulate bracketed paste: ESC[200~ ... ESC[201~
        pasted = "\x1b[200~/path/to/dropped.txt\x1b[201~"

        result = _run(
            clud_binary,
            "-p",
            "hello",
            "--",
            "--mock-read-stdin-ms",
            "3000",
            "--mock-report-file",
            str(report_file),
            env=mock_env,
            input_data=pasted,
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"

        report = self._parse_report(report_file, result.stdout, result.stderr)
        assert report["stdin"] is not None
        assert "/path/to/dropped.txt" in report["stdin"]
