"""Exit-stage attribution for the #594 timeout victims.

#594's Windows integration lane fails with a rotating victim, and the
partial-output capture in `run_clud` established that the *work* completes and
the process then fails to exit inside its budget. Attributing that needs a
trace written **as each teardown stage runs**: clud's `[clud] exit timing:`
summary prints only after the last stage, so a process the harness kills
mid-teardown emits nothing at all about where it was.

These tests pin the contract that makes the next occurrence self-diagnosing.
"""

from __future__ import annotations

import os
import subprocess
import tempfile

import pytest

from ._daemon_helpers import _read_exit_stages, run_clud

pytestmark = pytest.mark.integration


def test_exit_stage_file_records_each_stage_as_it_runs(clud_binary, mock_env):
    """`CLUD_EXIT_TIMING_FILE` gets a begin/done pair per teardown stage."""
    fd, trace = tempfile.mkstemp(prefix="clud-exit-stage-test-", suffix=".log")
    os.close(fd)
    try:
        env = dict(mock_env)
        env["CLUD_EXIT_TIMING_FILE"] = trace
        result = subprocess.run(
            [str(clud_binary), "-p", "hello"],
            capture_output=True,
            text=True,
            timeout=60,
            env=env,
        )
        assert result.returncode == 0, f"stderr: {result.stderr}"

        with open(trace, encoding="utf-8") as handle:
            body = handle.read()

        # scan_and_report is the host-wide originator scan #594 suspects, and
        # tracker_drop is the completion-port listener join it names alongside.
        assert "exit-stage begin scan_and_report" in body, body
        assert "exit-stage done scan_and_report=" in body, body
        assert "exit-stage begin tracker_drop" in body, body

        # Every stage entered on a clean run must also complete; an unmatched
        # begin here would mean the trace itself is lying.
        assert "STALLED IN" not in _read_exit_stages(trace)
    finally:
        if os.path.exists(trace):
            os.unlink(trace)


def test_exit_stage_trace_stays_off_stderr(clud_binary, mock_env):
    """The file sink must not leak to stderr.

    The many tests asserting clean stderr are why this sink is a file at all;
    routing it to stderr is what `CLUD_EXIT_TIMING=1` is for.
    """
    fd, trace = tempfile.mkstemp(prefix="clud-exit-stage-quiet-", suffix=".log")
    os.close(fd)
    try:
        env = dict(mock_env)
        env["CLUD_EXIT_TIMING_FILE"] = trace
        result = subprocess.run(
            [str(clud_binary), "-p", "hello"],
            capture_output=True,
            text=True,
            timeout=60,
            env=env,
        )
        assert "exit-stage" not in result.stderr, result.stderr
        assert "exit-stage" not in result.stdout
    finally:
        if os.path.exists(trace):
            os.unlink(trace)


def test_read_exit_stages_flags_an_unterminated_stage():
    """An unmatched `begin` is reported as the stall site.

    This is the case the whole mechanism exists for and the one a green run
    can never exercise, so it is pinned directly.
    """
    fd, trace = tempfile.mkstemp(prefix="clud-exit-stage-stall-", suffix=".log")
    os.close(fd)
    try:
        with open(trace, "w", encoding="utf-8") as handle:
            handle.write(
                "exit-stage begin sweep_abandoned_at_exit\n"
                "exit-stage done sweep_abandoned_at_exit=0ms\n"
                "exit-stage begin scan_and_report\n"
            )
        rendered = _read_exit_stages(trace)
        assert "STALLED IN: scan_and_report" in rendered
        assert "sweep_abandoned_at_exit" not in rendered.split("STALLED IN")[1]
    finally:
        if os.path.exists(trace):
            os.unlink(trace)


def test_read_exit_stages_handles_a_process_that_never_reached_teardown():
    fd, trace = tempfile.mkstemp(prefix="clud-exit-stage-empty-", suffix=".log")
    os.close(fd)
    try:
        assert "never reached the exit path" in _read_exit_stages(trace)
    finally:
        if os.path.exists(trace):
            os.unlink(trace)


def test_run_clud_reports_exit_stages_on_timeout(clud_binary, mock_env):
    """A timed-out launch surfaces the trace in the failure message.

    `--mock-sleep-ms` is only meaningful against the mock agent, so this needs
    `mock_env`: without it clud resolves whatever `claude` is on PATH (nothing,
    on a CI runner), exits immediately, and never times out at all.
    """
    with pytest.raises(AssertionError) as excinfo:
        run_clud(
            [str(clud_binary), "-p", "hello", "--", "--mock-sleep-ms", "20000"],
            timeout=2.0,
            env=mock_env,
        )
    message = str(excinfo.value)
    assert "--- exit stages ---" in message
    assert "timed out after 2.0s" in message
