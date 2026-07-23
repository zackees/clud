"""Pure report assembly and budget comparison for the idle CPU harness."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

CPU_MARGIN = 0.20
EVENT_LINE_SLACK = 1


def _delta(after: float | int | None, before: float | int | None) -> float | int | None:
    if after is None or before is None:
        return None
    return round(max(0, after - before), 9)


def assemble_report(
    *,
    head: str,
    timestamp: str,
    sessions: int,
    window_secs: float,
    roles: Mapping[int, str],
    before: Mapping[int, Mapping[str, float | int | None]],
    after: Mapping[int, Mapping[str, float | int | None]],
    event_lines_before: int,
    event_lines_after: int,
) -> dict[str, Any]:
    """Build a stable, JSON-ready report from two synthetic or real samples."""
    per_process: list[dict[str, Any]] = []
    client_cpu_seconds = 0.0
    daemon_cpu_seconds = 0.0

    for pid, role in roles.items():
        start = before.get(pid, {})
        end = after.get(pid, {})
        cpu_seconds = _delta(end.get("cpu_seconds"), start.get("cpu_seconds"))
        ctx_switches = _delta(end.get("ctx_switches"), start.get("ctx_switches"))
        per_process.append(
            {
                "role": role,
                "pid": pid,
                "cpu_seconds": cpu_seconds,
                "ctx_switches": ctx_switches,
                "missing_at_end": pid not in after,
            }
        )
        if cpu_seconds is not None:
            if role == "daemon":
                daemon_cpu_seconds += float(cpu_seconds)
            else:
                client_cpu_seconds += float(cpu_seconds)

    return {
        "head": head,
        "timestamp": timestamp,
        "sessions": sessions,
        "window_secs": window_secs,
        "per_process": per_process,
        "totals": {
            "client_cpu_seconds": client_cpu_seconds,
            "daemon_cpu_seconds": daemon_cpu_seconds,
            "event_lines_appended": max(0, event_lines_after - event_lines_before),
        },
    }


def budget_violations(
    report: Mapping[str, Any], baseline: Mapping[str, Any]
) -> list[str]:
    """Return human-readable violations without doing I/O or exiting."""
    measured = report["totals"]
    expected = baseline["totals"]
    violations: list[str] = []

    for key in ("client_cpu_seconds", "daemon_cpu_seconds"):
        limit = float(expected[key]) * (1 + CPU_MARGIN)
        actual = float(measured[key])
        if actual > limit:
            violations.append(f"{key} {actual:.6f} exceeds {limit:.6f} (baseline +20%)")

    event_limit = int(expected["event_lines_appended"]) + EVENT_LINE_SLACK
    event_actual = int(measured["event_lines_appended"])
    if event_actual > event_limit:
        violations.append(
            f"event_lines_appended {event_actual} exceeds {event_limit} "
            f"(baseline +{EVENT_LINE_SLACK})"
        )
    return violations
