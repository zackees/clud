"""Integration test: F3 voice mode injects transcripts into the PTY (issue #13).

This test bypasses real Whisper transcription via the
``CLUD_VOICE_TEST_TRANSCRIPT`` env var, so it needs no model file and no
microphone. The mock-agent's PTY stdin is fed an F3 press followed by a
kitty F3 release sequence; the test asserts that clud writes the
test-transcript bytes into the PTY (because the release fires the stop
path, which queues a transcription, which then drains via
``on_tick`` → ``write_impl``).

We can't observe the bytes via the mock-agent's normal JSON report
because they arrive AFTER its stdin read window; instead we capture the
raw stdin bytes with ``--mock-stdin-raw-to <file>`` and grep for the
transcript text.

Why this test only runs in environments where the kitty path is wired
correctly: the runner has no actual terminal sending real release
sequences. We synthesize them by piping the bytes to clud's stdin, which
flows through the PTY pump's F3Observer. The Rust integration test in
``crates/clud-bin/tests/pty_behavior.rs`` already covers the observer in
isolation — this is the end-to-end smoke that the wiring from
observer → VoiceMode → PTY write is intact.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

pytestmark = pytest.mark.integration

_TIMEOUT = 30


def _run(
    clud: Path,
    *args: str,
    env: dict[str, str],
    input_data: bytes | None = None,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[bytes]:
    """Run clud with bytes-mode stdin. Voice events are raw escape
    sequences, so we need byte-fidelity over the pipe — `text=True` would
    re-encode and could mangle the kitty CSI bytes."""
    with tempfile.TemporaryDirectory() as temp_dir:
        launch = Path(temp_dir) / clud.name
        shutil.copy2(clud, launch)
        return subprocess.run(
            [str(launch), *args],
            capture_output=True,
            text=False,
            timeout=_TIMEOUT,
            env=env,
            input=input_data,
            cwd=cwd,
        )


@pytest.mark.skipif(
    sys.platform == "win32",
    reason=(
        "Windows ConPTY does not deliver kitty release sequences; the press "
        "is observed but the release relies on VAD auto-stop, which can't be "
        "exercised without a real mic. The Rust observer unit tests in "
        "session.rs cover the byte-level matcher on every platform."
    ),
)
def test_voice_mode_transcript_injects_into_pty(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    """End-to-end: F3 press + kitty release → CLUD_VOICE_TEST_TRANSCRIPT
    bytes appear in the PTY's stdin stream, proving the
    observer → VoiceMode → write_impl wiring is intact."""
    raw_stdin = tmp_path / "stdin_raw.bin"

    # CLUD_VOICE_TEST_TRANSCRIPT short-circuits Whisper. The mock-agent
    # captures whatever clud writes into the PTY so we can assert on it.
    env = dict(mock_env)
    env["CLUD_VOICE_TEST_TRANSCRIPT"] = "hello voice mode"

    # F3 press (SS3 form, broadly supported) immediately followed by kitty
    # F3 release. The release fires the stop path; the next on_tick drains
    # the worker and writes the transcript bytes back into the PTY.
    payload = b"\x1bOR\x1b[57346;1:3u"

    # Run clud with --pty against the mock-agent. The mock-agent will
    # record any bytes that arrive on its PTY stdin (the transcript) plus
    # whatever we sent before it stopped reading.
    result = _run(
        clud_binary,
        "--pty",
        "-p",
        "ignored",
        "--",
        "--mock-read-stdin-ms",
        "1500",
        "--mock-stdin-raw-to",
        str(raw_stdin),
        env=env,
        input_data=payload + b"\n",
    )

    assert result.returncode == 0, (
        f"clud exited {result.returncode}; stderr:\n{result.stderr!r}"
    )
    assert raw_stdin.is_file(), (
        f"mock-agent stdin capture file missing; stderr:\n{result.stderr!r}"
    )
    captured = raw_stdin.read_bytes()
    assert b"hello voice mode" in captured, (
        "transcript bytes did not reach the PTY; "
        f"captured stdin:\n{captured!r}\nstderr:\n{result.stderr!r}"
    )
