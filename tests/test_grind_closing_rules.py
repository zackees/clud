"""Guard the /grind closing rules in the bundled skill assets (#1403)."""

from __future__ import annotations

from pathlib import Path

SKILLS = Path(__file__).resolve().parents[1] / "crates/clud-bin/assets/skills"


def _read(name: str) -> str:
    return (SKILLS / name / "SKILL.md").read_text(encoding="utf-8")


def test_integrate_uses_refs_off_the_default_branch() -> None:
    text = _read("grind-integrate")
    assert "Refs #<id>" in text
    assert "Closes #<id>" in text
    assert "default branch" in text


def test_intake_does_not_hand_close_the_meta_issue() -> None:
    text = _read("grind-intake")
    assert "the parent is closed by `/grind`" not in text


def test_bare_intake_skips_issues_on_a_feature_branch() -> None:
    text = _read("grind-intake")
    assert "grind:on-feature" not in text
    assert "feature PR's `Closes` line" in text
    assert "grind:followup" in text


def test_intake_never_says_grind_closes_issues() -> None:
    assert "closed by `/grind`" not in _read("grind-intake")


def test_integrate_goal_prs_use_refs() -> None:
    assert "Refs #" in _read("grind-integrate")


def test_land_marks_feature_issues_without_a_label() -> None:
    text = _read("grind-land")
    assert "grind:on-feature" not in text
    assert "gh label create" not in text
    assert "<!-- grind:v1 feature-pr=" in text


def test_router_and_cron_run_reconcile() -> None:
    assert "clud grind reconcile" in _read("grind")
    assert "clud grind reconcile" in _read("grind-cron")


def test_triage_knows_feature_branch_issues_by_marker() -> None:
    text = _read("clud-issue-triage")
    assert "grind:on-feature" not in text
    assert "<!-- grind:v1 feature-pr=#N" in text


def test_git_playbook_protects_grind_branches() -> None:
    assert "grind/*" in _read("clud-git")
