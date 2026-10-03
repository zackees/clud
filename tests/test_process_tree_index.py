"""#1738: the test suite's tree kill must not follow a recycled parent PID."""

from __future__ import annotations

from tests.process import descendants, index_children

RUNNER, PYTEST, RECYCLED = 100, 101, 50


def test_older_process_is_not_a_child_of_its_recycled_parent_pid() -> None:
    # The runner's real parent (PID 50) died long ago; a clud daemon started
    # at t=900 was later handed PID 50. Its tree must not reach the runner.
    children = index_children(
        [
            (RUNNER, RECYCLED, 100.0),
            (PYTEST, RUNNER, 200.0),
            (RECYCLED, PYTEST, 900.0),
        ]
    )
    assert descendants(children, RECYCLED) == []
    assert descendants(children, RUNNER) == [PYTEST, RECYCLED]


def test_unknown_or_dead_parent_links_are_dropped() -> None:
    children = index_children([(10, None, 1.0), (11, 10, None), (12, 99, 5.0), (13, 13, 5.0)])
    assert children == {}


def test_same_instant_child_is_kept_and_a_cycle_terminates() -> None:
    children = index_children([(1, 2, 5.0), (2, 1, 5.0)])
    assert descendants(children, 1) == [2]
