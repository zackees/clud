"""End-to-end test for the embedded WASM runtime."""

from __future__ import annotations

import hashlib
import os
import sys
from pathlib import Path

from tests import process

FIXTURE_DIR = Path(__file__).parent / "fixtures" / "wasm"
WASM_FIXTURE = FIXTURE_DIR / "hello.wasm"
WASM_FIXTURE_SHA256 = "558cb22ebf48846ea261fec0cd1772575d1bc1eded50f0e7f3e959d31b2ab1b4"


def _clud_binary() -> str:
    """Find the clud binary in the venv."""
    env_binary = os.environ.get("CLUD_TEST_BINARY")
    if env_binary and Path(env_binary).is_file():
        return env_binary

    venv = Path(sys.executable).parent
    if sys.platform == "win32":
        candidate = venv / "clud.exe"
    else:
        candidate = venv / "clud"
    if candidate.is_file():
        return str(candidate)
    return "clud"


CLUD = _clud_binary()


def test_wasm_fixture_is_deterministic_and_zig_free() -> None:
    """Keep the checked-in binary reviewable from its adjacent WAT source.

    Regenerate with ``wasm-tools parse tests/fixtures/wasm/hello.wat -o
    tests/fixtures/wasm/hello.wasm`` and update ``WASM_FIXTURE_SHA256``.
    The fixture must be usable without a Python Zig package or a C/C++ compiler.
    """
    assert (FIXTURE_DIR / "hello.wat").is_file()
    assert WASM_FIXTURE.is_file()
    assert hashlib.sha256(WASM_FIXTURE.read_bytes()).hexdigest() == WASM_FIXTURE_SHA256


def test_wasm_hello_world() -> None:
    result = process.run(
        [CLUD, "wasm", str(WASM_FIXTURE)],
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode == 0, result.stderr
    assert "hello from wasm" in result.stdout
