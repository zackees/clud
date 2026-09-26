"""Guard the feature-branch-mode contract in the bundled /grind assets (#1410, #1393).

The harness suites (`tests/harness/test_grind_feature.py`,
`test_grind_reconcile.py`) run the real workflow; these checks pin the text
the scripted model cannot exercise: who adds a goal's `Closes` line, when the
router readies the feature PR, and what Finish may delete.
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "crates/clud-bin/assets"


def _skill(name: str) -> str:
    return (ASSETS / "skills" / name / "SKILL.md").read_text(encoding="utf-8")


def _workflow() -> str:
    return (ASSETS / "workflows/grind-run.js").read_text(encoding="utf-8")


def _function(source: str, name: str) -> str:
    """The source of `const <name> = ...` up to the next top-level `const`."""
    start = source.index(f"const {name} =")
    end = source.find("\nconst ", start + 1)
    return source[start : end if end != -1 else len(source)]


def test_only_the_lander_adds_a_goals_closes_line_after_it_lands() -> None:
    # A Closes line added when the goal PR opens would close an issue whose
    # goal never landed once the feature PR merges (#1393).
    integrate = _skill("grind-integrate")
    assert "Do not edit the feature PR" in integrate
    assert "gh pr edit <feature-pr>" not in integrate
    note = _function(_workflow(), "featureIntegrateNote")
    assert "gh pr edit" not in note
    assert "Do not edit the feature PR" in note
    land = _function(_workflow(), "featureLandNote")
    assert "Once it has merged" in land
    assert "gh pr edit ${fpr}" in land
    assert "Closes #${g.id}" in land
    assert "Only you add a goal's `Closes` line" in _skill("grind-land")


def test_lander_records_label_and_marker_on_landing() -> None:
    source = _workflow()
    assert "const ON_FEATURE = 'grind:on-feature'" in source
    land = _function(source, "featureLandNote")
    assert "gh label create ${ON_FEATURE} --force" in land
    assert "--add-label ${ON_FEATURE}" in land
    assert "<!-- grind:v1 feature-pr=#${fpr} branch=${FEATURE.branch}" in land
    assert "run=${FEATURE_RUN_ID" in land


def test_feature_branch_name_carries_the_run_id() -> None:
    # S6: two runs on one meta issue never collide.
    text = _skill("grind")
    assert "grind/meta-<M>-<run-id>" in text


def test_decide_later_readies_but_never_merges() -> None:
    text = _skill("grind")
    later = re.search(r"- `later`:(.*?)- `comment`:", text, re.S)
    assert later, text
    assert "gh pr ready" in later.group(1)
    assert "never merge" in later.group(1)


def test_finish_keeps_grind_branches_and_pushes_worktrees() -> None:
    text = _skill("grind")
    finish = text[text.index("## 5. Finish") :]
    assert "push every worktree before" in finish
    assert "Never delete a `grind/*` branch while its feature PR is" in finish
    # run.json goes last: the hook reads it to guard grind/* branches.
    assert finish.index("Remove every") < finish.index("remove\n   `.clud/grind/run.json`")


def test_feature_goals_never_merge_main_by_rebase() -> None:
    integrate = _skill("grind-integrate")
    assert "merge (never\n   rebase) `origin/<main>`" in integrate
    assert "Never rebase the feature branch" in _function(_workflow(), "featureIntegrateNote")


def test_merge_commit_design_decision_exists_once() -> None:
    text = (ROOT / "docs/DESIGN_DECISIONS.md").read_text(encoding="utf-8")
    heads = re.findall(r"^## DD-\d+: The /grind feature lands as a merge commit", text, re.M)
    assert len(heads) == 1, heads
