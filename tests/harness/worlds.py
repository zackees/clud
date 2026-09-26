"""Canned fake-GitHub worlds for the /grind harness tests (#1402).

Each function returns a fresh ``fake_gh`` state dict (write it with
``Harness.write_gh_state``). Native sub-issue links are kept consistent in
both directions: every ``sub_issues`` entry's issue has ``parent`` pointing
back, and every issue with a ``parent`` is listed in that parent's
``sub_issues``.
"""

from __future__ import annotations

from typing import Any

GRIND_MARKER = "<!-- clud-grind -->"


def _issue(
    title: str,
    body: str = "",
    *,
    labels: list[str] | None = None,
    parent: int | None = None,
) -> dict[str, Any]:
    return {
        "title": title,
        "body": body,
        "state": "open",
        "labels": list(labels or []),
        "comments": [],
        "sub_issues": [],
        "parent": parent,
    }


def _world(issues: dict[int, dict[str, Any]]) -> dict[str, Any]:
    """Wrap `issues`, filling each parent's `sub_issues` from the children."""
    for number, issue in sorted(issues.items()):
        parent = issue["parent"]
        if parent is not None:
            issues[parent]["sub_issues"].append({"number": number, "state": issue["state"]})
    return {
        "repo": "o/r",
        "default_branch": "main",
        "issues": {str(n): issue for n, issue in issues.items()},
        "prs": [],
        "next_id": 1000,
    }


def single_change() -> dict[str, Any]:
    return _world({1: _issue("fix the typo in README", "README says 'teh'; make it 'the'.")})


def multi_part() -> dict[str, Any]:
    body = (
        "Several parts:\n\n"
        "1. add a --verbose flag\n"
        "2. log each step when verbose\n"
        "3. document the flag in README\n"
    )
    return _world({1: _issue("verbose mode", body)})


def meta_with_sub_issues(n: int = 3) -> dict[str, Any]:
    issues = {1: _issue("meta: cleanup", "Tracked as sub-issues.")}
    for i in range(n):
        issues[2 + i] = _issue(f"cleanup part {i + 1}", f"do part {i + 1}", parent=1)
    return _world(issues)


def task_list_meta() -> dict[str, Any]:
    body = "Task list:\n\n- [ ] #2\n- [ ] #3\n- [ ] #4\n"
    return _world(
        {
            1: _issue("meta: task list", body),
            2: _issue("task a", "do a"),
            3: _issue("task b", "do b"),
            4: _issue("task c", "do c"),
        }
    )


def mixed() -> dict[str, Any]:
    body = "Sub-issues #2 and #3, plus:\n\n- [ ] #4\n- [ ] #5\n"
    return _world(
        {
            1: _issue("meta: mixed", body),
            2: _issue("native a", "do a", parent=1),
            3: _issue("native b", "do b", parent=1),
            4: _issue("listed c", "do c"),
            5: _issue("listed d", "do d"),
        }
    )


def regroupable() -> dict[str, Any]:
    issues = {1: _issue("meta: backlog", "A flat backlog across two themes.")}
    number = 2
    for theme in ("docs", "cli"):
        for i in range(4):
            issues[number] = _issue(f"{theme}: item {i + 1}", f"{theme} work {i + 1}", parent=1)
            number += 1
    return _world(issues)


def with_user_sub_meta() -> dict[str, Any]:
    return _world(
        {
            1: _issue("meta: top", "Top-level meta."),
            2: _issue("leaf a", "do a", parent=1),
            3: _issue("meta: user-made group", "A group the user made.", parent=1),
            4: _issue("leaf b", "do b", parent=3),
            5: _issue("leaf c", "do c", parent=3),
        }
    )


def with_grind_sub_meta() -> dict[str, Any]:
    return _world(
        {
            1: _issue("meta: top", "Top-level meta."),
            2: _issue("leaf a", "do a", parent=1),
            3: _issue(
                "grind: group",
                f"{GRIND_MARKER}\nGrouped by /grind.",
                labels=["grind:meta"],
                parent=1,
            ),
            4: _issue("leaf b", "do b", parent=3),
            5: _issue("leaf c", "do c", parent=3),
        }
    )


ALL = {
    "single_change": single_change,
    "multi_part": multi_part,
    "meta_with_sub_issues": meta_with_sub_issues,
    "task_list_meta": task_list_meta,
    "mixed": mixed,
    "regroupable": regroupable,
    "with_user_sub_meta": with_user_sub_meta,
    "with_grind_sub_meta": with_grind_sub_meta,
}
