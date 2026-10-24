"""#1893: a PR into a base that no workflow triggers on fails fast."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

TOOL = (
    Path(__file__).resolve().parents[1]
    / "crates"
    / "clud-bin"
    / "assets"
    / "tools"
    / "github"
    / "pr_merge_watch.py"
)


@pytest.fixture(scope="module")
def watcher():
    spec = importlib.util.spec_from_file_location("pr_merge_watch_1893", TOOL)
    module = importlib.util.module_from_spec(spec)
    sys.modules["pr_merge_watch_1893"] = module
    spec.loader.exec_module(module)
    return module


MASTER_ONLY = """name: PR
on:
  workflow_dispatch:
  pull_request:
    branches: [master]
"""

FEATURE = "grind/meta-4-7c2e"


@pytest.mark.parametrize(
    ("branches", "ignore", "expected"),
    [
        (["master"], None, False),
        (["**"], None, True),
        (None, None, True),
        (None, ["master"], True),
        (["grind/*"], None, True),
        (["grind/*/x"], None, False),
        (["release/*"], None, False),
        (["grind/**"], None, True),
        (["**", "!grind/**"], None, False),
    ],
)
def test_branch_filters(watcher, branches, ignore, expected) -> None:
    assert watcher.branch_filter_matches(branches, ignore, FEATURE) is expected


def test_a_master_only_workflow_does_not_trigger_but_is_dispatchable(watcher) -> None:
    assert watcher.triggers_for_base({"pr.yaml": MASTER_ONLY}, FEATURE) == {
        "pr_workflows": [],
        "dispatchable": ["pr.yaml"],
    }
    on_master = watcher.triggers_for_base({"pr.yaml": MASTER_ONLY}, "master")
    assert on_master["pr_workflows"] == ["pr.yaml"]


@pytest.mark.parametrize(
    "text",
    [
        "on: [push, pull_request]\n",
        "on: pull_request\n",
        "on:\n  pull_request:\n  push:\n    branches: [main]\n",
        "'on':\n  pull_request_target:\n    branches:\n      - '**'\n",
        "on:\n  pull_request:\n    branches-ignore:\n      - master\n",
    ],
)
def test_common_trigger_shapes_match_a_feature_base(watcher, text) -> None:
    assert watcher.triggers_for_base({"ci.yml": text}, FEATURE)["pr_workflows"] == ["ci.yml"]


def test_an_unrecognized_shape_is_assumed_to_trigger(watcher) -> None:
    result = watcher.triggers_for_base({"odd.yml": "jobs: {}\n"}, FEATURE)
    assert result["pr_workflows"] == ["odd.yml"]


def test_the_watcher_reports_no_trigger_for_base_immediately(watcher, monkeypatch) -> None:
    monkeypatch.setattr(watcher, "fetch_workflow_presence", lambda *a: True)
    monkeypatch.setattr(watcher, "fetch_workflow_texts", lambda *a: {"pr.yaml": MASTER_ONLY})
    monkeypatch.setattr(watcher, "fetch_head_commit_message", lambda *a: "feat: x")
    snapshot = watcher.PRSnapshot(10, "OPEN", "MERGEABLE", "abc", FEATURE, "grind/5")
    assert watcher.immediate_no_checks_reason("o/r", snapshot) == "no_trigger_for_base"
    snapshot.base_ref = "master"
    assert watcher.immediate_no_checks_reason("o/r", snapshot) is None
