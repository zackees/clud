"""Integration coverage for the interactive (PTY) `clud attach` loop."""

from __future__ import annotations

import time
from pathlib import Path

import pytest
from running_process import PseudoTerminalProcess

from ._daemon_helpers import (
    DETACH_EXIT_TIMEOUT,
    kill_daemon_for_session,
    launch_detached,
    managed_env,
    wait_for_exit,
)

pytestmark = pytest.mark.integration

# How long the mock backend runs before it exits on its own. Long enough
# that the attach below connects while the session is still alive, so the
# attach is sitting in its input loop when the exit arrives.
SESSION_RUNTIME_MS = 6000


def test_interactive_attach_returns_the_session_exit_code_when_it_ends(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    """#1448: an interactive attach must end when its session does.

    The attach runs on a real pseudo-terminal, so it takes the raw-input
    loop. Nothing is typed and stdin never reaches EOF, as on a real
    terminal. Before the fix the loop never read the exit code the output
    reader recorded, so the attach waited for Ctrl+C until the timeout.
    """
    state_dir = tmp_path / "daemon-state"
    env = managed_env(mock_env, state_dir)
    launcher, session_id = launch_detached(
        clud_binary,
        env,
        "--codex",
        "--",
        "--mock-sleep-ms",
        str(SESSION_RUNTIME_MS),
        "--mock-exit-code",
        "7",
    )
    attach: PseudoTerminalProcess | None = None
    try:
        assert wait_for_exit(launcher, timeout=DETACH_EXIT_TIMEOUT) == 0
        started = time.monotonic()
        attach = PseudoTerminalProcess(
            [str(clud_binary), "attach", session_id],
            env=env,
        )
        try:
            exit_code = attach.wait(timeout=SESSION_RUNTIME_MS / 1000 + 30)
        except Exception as err:
            raise AssertionError(
                "interactive attach did not return after its session exited; "
                f"output so far: {attach.output_text[-2000:]!r}"
            ) from err
        elapsed = time.monotonic() - started
        assert exit_code == 7, attach.output_text[-2000:]
        # The attach must have joined a live session, or the handshake's own
        # `Exited` answered it and the input loop never ran.
        assert elapsed > 1.0, f"attach returned after {elapsed:.2f}s"
    finally:
        if attach is not None and attach.poll() is None:
            attach.kill()
        kill_daemon_for_session(state_dir, session_id)
