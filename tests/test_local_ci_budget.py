"""Local act runs are one pre-push check, not the edit loop (#1715).

On 2026-10-02 a one-module fix spent about 50 minutes in
`bosn run --task act-ci-linux`. It waited about 27 minutes behind other
sessions on a bosn daemon that runs one job at a time (zackees/bosn#358),
spent 9 minutes testing another checkout (#1594), and lost two runs to
kills, one of them a cross-session `pkill`-style kill (zackees/bosn#357).
The policy has to bound how agents use local act, and CLAUDE.md has to
point at it.
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CI_MD = ROOT / "docs" / "architecture" / "ci.md"
CLAUDE_MD = ROOT / "CLAUDE.md"


def _flat(path: Path) -> str:
    """The file's text with wrapped lines joined."""
    return re.sub(r"\s+", " ", path.read_text(encoding="utf-8"))


def test_ci_doc_states_the_local_act_budget() -> None:
    assert "### Local CI budget" in CI_MD.read_text(encoding="utf-8")
    text = _flat(CI_MD)
    for rule in (
        "at most once per change",
        "CI of record",
        "Don't wait in the bosn queue",
        "bosn job cancel",
        "Never restart a running act job",
        "pkill",
    ):
        assert rule in text, f"ci.md lost the local CI budget rule {rule!r}"


def test_ci_doc_no_longer_requires_a_local_rerun_loop() -> None:
    assert "rerun it to green before pushing" not in _flat(CI_MD)


def test_claude_md_points_agents_at_the_budget() -> None:
    text = _flat(CLAUDE_MD)
    assert "docs/architecture/ci.md#local-ci-budget" in text
    assert "at most once per change" in text
    assert "pkill" in text
