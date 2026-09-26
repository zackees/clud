"""Guard the /grind input-routing rules in the bundled skill assets (#1405)."""

from __future__ import annotations

from pathlib import Path

SKILLS = Path(__file__).resolve().parents[1] / "crates/clud-bin/assets/skills"


def _read(name: str) -> str:
    return (SKILLS / name / "SKILL.md").read_text(encoding="utf-8")


def test_intake_classifies_meta_issues_with_the_bundled_tool() -> None:
    text = _read("grind-intake")
    assert "is_meta_issue.py" in text


def test_intake_converts_multi_part_issues_into_a_meta_issue() -> None:
    text = _read("grind-intake")
    assert "Tracks #" in text
    assert "Split into meta" in text
    assert "sub_issue_id" in text


def test_intake_verifies_links_and_reports_partial_state() -> None:
    text = _read("grind-intake")
    assert "both ways" in text
    assert "partial" in text.lower()


def test_intake_refuses_single_issues_with_do() -> None:
    text = _read("grind-intake")
    assert "run `/do N`" in text
    assert "Nothing to do" in text


def test_router_refuses_single_issues_with_do() -> None:
    text = _read("grind")
    assert "run `/do N`" in text
