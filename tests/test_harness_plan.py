"""How `ci/run_bundle.py` splits the one integration harness into processes (#1726).

#1726 folded six integration harnesses into one, `integration`. CI still runs
each category in a process of its own, as when they were separate binaries,
and every `pty` test in its own pseudo-terminal (#691, #1310).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from ci import harness_plan as plan


@pytest.mark.parametrize(
    ("file", "name"),
    [
        ("integration-ad3bdf1f57219854", "integration"),
        ("integration-ad3bdf1f57219854.exe", "integration"),
        ("clud-0", "clud"),
        ("mock_agent-1a2b", "mock_agent"),
        ("integration", "integration"),
    ],
)
def test_harness_name_drops_the_hash_and_extension(file: str, name: str) -> None:
    assert plan.harness_name(Path(file)) == name


@pytest.mark.parametrize("file", ["integration-1a2b", "integration-1a2b.exe"])
def test_only_the_integration_harness_is_split(file: str) -> None:
    assert plan.is_split(Path(file))


@pytest.mark.parametrize("file", ["clud-1a2b", "cli-9f8e7d.exe", "pty-1234", "integration"])
def test_other_harnesses_run_whole(file: str) -> None:
    assert not plan.is_split(Path(file))


NAMES = [
    "api::api_turn_controller::turn_runs",
    "pty::pty_pump::forwards_stdin",
    "reaper::orphan_reap::sweeps",
    "pty::pty_behavior::resizes",
    "api::api_session_lifecycle::starts",
    "cli::shell_completion_guard::counts_api::functions",
]


def test_each_category_is_one_process_and_each_pty_test_its_own_terminal() -> None:
    runs = plan.split_runs(NAMES)
    assert [(r.category, r.tests, r.terminal) for r in runs] == [
        (
            "api",
            ("api::api_turn_controller::turn_runs", "api::api_session_lifecycle::starts"),
            False,
        ),
        ("cli", ("cli::shell_completion_guard::counts_api::functions",), False),
        ("pty", ("pty::pty_pump::forwards_stdin",), True),
        ("pty", ("pty::pty_behavior::resizes",), True),
        ("reaper", ("reaper::orphan_reap::sweeps",), False),
    ]


def test_runs_select_exact_names_so_a_substring_cannot_leak_across_categories() -> None:
    api = plan.split_runs(NAMES)[0]
    assert api.argv(["harness"]) == [
        "harness",
        "--exact",
        "api::api_turn_controller::turn_runs",
        "api::api_session_lifecycle::starts",
    ]


def test_a_large_category_is_batched_under_the_windows_command_line_limit() -> None:
    names = [f"reaper::m::test_{i:04d}_{'x' * 60}" for i in range(400)]
    runs = plan.split_runs(names)
    assert len(runs) > 1
    assert all(r.category == "reaper" and not r.terminal for r in runs)
    assert [n for r in runs for n in r.tests] == names
    assert all(sum(len(n) + 1 for n in r.tests) <= plan.MAX_FILTER_CHARS for r in runs)
