"""Shared pytest configuration for clud tests."""

from __future__ import annotations

import os

import pytest


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Skip integration + perf_budget tests unless explicitly enabled.

    - ``integration`` tests run when ``CLUD_INTEGRATION_TESTS=1`` *or* the
      caller passed ``-m integration`` on the pytest command line.
    - ``perf_budget`` tests (#266) run when ``CLUD_PERF_BUDGET=1`` *or*
      the caller passed ``-m perf_budget``. They're heavy (1k-iteration
      loops) and host-sensitive, so the default ``bash test`` run skips
      them. The unmarked smoke variants always run.
    """
    selected_markers = (config.getoption("markexpr") or "")

    run_integration = (
        os.environ.get("CLUD_INTEGRATION_TESTS", "").lower() in {"1", "true", "yes"}
        or "integration" in selected_markers
    )
    run_perf = (
        os.environ.get("CLUD_PERF_BUDGET", "").lower() in {"1", "true", "yes"}
        or "perf_budget" in selected_markers
    )

    skip_integration = pytest.mark.skip(reason="set CLUD_INTEGRATION_TESTS=1 to run")
    skip_perf = pytest.mark.skip(
        reason="set CLUD_PERF_BUDGET=1 or pass `-m perf_budget` to run"
    )
    for item in items:
        if "integration" in item.keywords and not run_integration:
            item.add_marker(skip_integration)
        if "perf_budget" in item.keywords and not run_perf:
            item.add_marker(skip_perf)
