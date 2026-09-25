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


def test_bare_intake_skips_grind_labelled_issues() -> None:
    text = _read("grind-intake")
    assert "grind:on-feature" in text
    assert "grind:followup" in text
