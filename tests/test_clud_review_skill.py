"""Regression contract for the one-reviewer budget in bundled clud-review."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "crates" / "clud-bin" / "assets" / "skills" / "clud-review" / "SKILL.md"
TRANSCRIPT = ROOT / "tests" / "fixtures" / "clud_review_multi_bucket_retry.txt"


def test_multi_bucket_retry_review_has_one_invocation_wide_agent_budget() -> None:
    """#781: buckets and retries are prompt sections, never new agents."""
    body = SKILL.read_text(encoding="utf-8")
    transcript = TRANSCRIPT.read_text(encoding="utf-8")

    assert "Changed buckets: rust, python, docs" in transcript
    assert "agent_budget=1" in body
    assert "review agents launched: 1" in body
    assert "agent_budget_exhausted" in body
    assert "one primary reviewer" in body
    assert "dispatch the bucket's review to that subagent" not in body
    assert "Subagents are *executed*" not in body
    # The caller-side half of this contract used to be asserted against
    # clud-fix and clud-pr. Both were retired in favour of the harness /goal
    # hook, so clud-review is now the sole owner of the budget rule.
