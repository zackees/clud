"""Guard the /grind input-routing rules in the bundled skill assets (#1405)."""

from __future__ import annotations

from pathlib import Path

SKILLS = Path(__file__).resolve().parents[1] / "crates/clud-bin/assets/skills"


def _read(name: str) -> str:
    return (SKILLS / name / "SKILL.md").read_text(encoding="utf-8")


def test_intake_classifies_meta_issues_with_the_bundled_tool() -> None:
    text = _read("grind-intake")
    assert "is_meta_issue.py" in text
    # A `"$CLUD_EXE"` program word is refused by clud-cmd-scan's rm identity
    # check, so intake runs the tool through a plain `clud`.
    assert "`clud tool run github/is_meta_issue.py <N>`" in text
    assert '"$CLUD_EXE" tool run github/is_meta_issue.py' not in text


def test_intake_never_guesses_meta_on_a_tool_failure() -> None:
    text = _read("grind-intake")
    assert "exit 1 or 2" in text
    assert 'do not guess "not meta"' in text


def test_intake_converts_multi_part_issues_into_a_meta_issue() -> None:
    text = _read("grind-intake")
    assert "Tracks #" in text
    assert "Split into meta #<meta>; closes when" in text
    assert "sub_issue_id" in text
    # The conversion question shows the proposed children and its two options.
    assert "lists the proposed children" in text
    assert "**Convert to a meta issue**" in text
    assert "**Abort**" in text


def test_intake_creates_children_before_the_meta_issue() -> None:
    text = _read("grind-intake")
    # R11: a failed child never leaves an orphaned meta issue behind.
    assert "first create every child" in text
    assert "Only when every child exists, create the" in text
    assert "only when all exist, create the meta" in text


def test_intake_verifies_links_and_reports_partial_state() -> None:
    text = _read("grind-intake")
    assert "both ways" in text
    assert "/parent" in text
    assert "partial" in text.lower()


def test_intake_refuses_single_issues_with_do() -> None:
    text = _read("grind-intake")
    assert "run `/do N`" in text
    assert "(or `clud do N`) instead." in text
    assert "Nothing to do" in text


def test_intake_reads_task_list_ref_states() -> None:
    # The tool reports task-list refs without a state; "nothing to do" needs one.
    text = _read("grind-intake")
    assert "gh issue view <ref> --json state" in text


def test_router_refuses_single_issues_with_do() -> None:
    text = _read("grind")
    assert "run `/do N`" in text
