"""pr_merge_watch: panic lines carry a thread id on current Rust (issue #1623).

Rust now prints `thread '<name>' (<tid>) panicked at ...`. The #1616 skip for
panics inside passing tests must recognise that shape, and the fixtures here
use real GitHub Actions log lines (timestamp prefix, ANSI colour) fed through
the same normalisation `fetch_failure_log` applies.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "crates" / "clud-bin" / "assets" / "tools" / "github" / "pr_merge_watch.py"

TS = "2026-09-30T04:02:32.2188931Z "

PASSING_PANIC_THEN_PYTEST_FAILURE = "\n".join(
    TS + line
    for line in [
        "running 3 tests",
        "thread 'pty_pump::raw_pump_restores_raw_mode_on_panic' (12080) panicked at "
        "crates/clud-bin/tests/pty/pty_pump.rs:512:13:",
        "deliberate panic",
        "test pty_pump::raw_pump_restores_raw_mode_on_panic ... ok",
        "test result: ok. 3 passed; 0 failed; 0 ignored",
        "\x1b[31mFAILED\x1b[0m tests/test_cli.py::test_dry_run - AssertionError: assert 1 == 0",
        "\x1b[31m========================= 1 failed, 40 passed in 3.21s =========\x1b[0m",
    ]
)

GENUINE_PANIC = "\n".join(
    TS + line
    for line in [
        "running 2 tests",
        "thread 'parser::rejects_empty' (4711) panicked at crates/clud-bin/src/parser.rs:12:5:",
        "assertion failed: result.is_err()",
        "test parser::rejects_empty ... FAILED",
        "test parser::other ... ok",
        "test result: FAILED. 1 passed; 1 failed; 0 ignored",
    ]
)


@pytest.fixture
def watcher():
    name = "clud_test_pr_merge_watch_1623"
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


def _normalized(watcher, raw: str) -> str:
    return watcher.normalize_log(watcher.strip_ansi(raw))


def test_panic_with_thread_id_inside_passing_test_is_skipped(watcher) -> None:
    got = watcher.first_error_line(_normalized(watcher, PASSING_PANIC_THEN_PYTEST_FAILURE))
    assert got == "FAILED tests/test_cli.py::test_dry_run - AssertionError: assert 1 == 0"


def test_genuine_panic_with_thread_id_is_reported(watcher) -> None:
    got = watcher.first_error_line(_normalized(watcher, GENUINE_PANIC))
    assert got == (
        "thread 'parser::rejects_empty' (4711) panicked at crates/clud-bin/src/parser.rs:12:5:"
    )


def test_panic_thread_pattern_captures_name_with_and_without_id(watcher) -> None:
    for line in (
        "thread 'a::b' (12080) panicked at x.rs:1:1:",
        "thread 'a::b' panicked at x.rs:1:1:",
    ):
        m = watcher.PANIC_THREAD.match(line)
        assert m is not None
        assert m.group(1) == "a::b"


def test_colored_timestamped_failed_line_survives_normalization(watcher) -> None:
    raw = TS + "\x1b[31mFAILED\x1b[0m tests/test_x.py::test_y - assert False"
    assert watcher.first_error_line(_normalized(watcher, raw)) == (
        "FAILED tests/test_x.py::test_y - assert False"
    )
