"""pr_merge_watch: first-error line must skip panics in passing tests (issue #1616).

A test that deliberately panics (and catches it) prints a `thread '...'
panicked at` line to stderr and then reports `test ... ok`. That panic is not
why the job failed; the first-error line must point at the real failure
instead, while a panic that genuinely fails a test is still reported.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

PASSING_PANIC_THEN_PYTEST_FAILURE = """\
running 3 tests
thread 'pty_pump::raw_pump_restores_raw_mode_on_panic' panicked at src/pty_pump.rs:88:9:
deliberate panic
test pty_pump::raw_pump_restores_raw_mode_on_panic ... ok
test pty_pump::other ... ok
test result: ok. 3 passed; 0 failed; 0 ignored
============================= test session starts ==============================
FAILED tests/test_cli.py::test_dry_run - AssertionError: assert 1 == 0
========================= 1 failed, 40 passed in 3.21s =========================
"""

GENUINE_PANIC = """\
running 2 tests
thread 'parser::rejects_empty' panicked at crates/clud-bin/src/parser.rs:12:5:
assertion failed: result.is_err()
test parser::rejects_empty ... FAILED
test parser::other ... ok
failures:
    parser::rejects_empty
test result: FAILED. 1 passed; 1 failed; 0 ignored
"""


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1616"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


def test_panic_inside_passing_test_is_not_the_first_error(watcher) -> None:
    got = watcher.first_error_line(PASSING_PANIC_THEN_PYTEST_FAILURE)
    assert got == "FAILED tests/test_cli.py::test_dry_run - AssertionError: assert 1 == 0"


def test_genuine_panic_that_fails_a_test_is_still_reported(watcher) -> None:
    got = watcher.first_error_line(GENUINE_PANIC)
    assert got.startswith("thread 'parser::rejects_empty' panicked at")


def test_explicit_cargo_failure_markers_are_recognized(watcher) -> None:
    got = watcher.first_error_line("test result: FAILED. 0 passed; 1 failed")
    assert got.startswith("test result: FAILED")
    got = watcher.first_error_line("--- FAILED: TestThing (0.01s)")
    assert got == "--- FAILED: TestThing (0.01s)"
