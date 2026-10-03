"""Local CI runs are one pre-push check, not the edit loop (#1715).

On 2026-10-02 a one-module fix spent about 50 minutes in
`bosn run --task act-ci-linux`. It waited about 27 minutes behind other
sessions on a bosn daemon that runs one job at a time (zackees/bosn#358),
spent 9 minutes testing another checkout (#1594), and lost two runs to
kills, one of them a cross-session `pkill`-style kill (zackees/bosn#357).
The policy has to bound how agents use local act, and CLAUDE.md has to
point at it.

Local CI now runs through `bosn ci` -> act2 (#1740): one isolated engine per
run, so concurrent runs share no containers or action checkouts, and bosn
0.1.8 is the first release that runs act2.
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
        "bosn ci cancel",
        "Never restart a running run",
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


def test_docs_require_a_bosn_that_runs_act2() -> None:
    # bosn < 0.1.8 runs stock nektos/act: no runner parity, no overlay (#1740).
    assert "bosn>=0.1.8" in _flat(CI_MD)
    assert "bosn 0.1.8 or newer" in _flat(CLAUDE_MD)


def test_the_private_act_wrapper_is_gone() -> None:
    # bosn ci owns act, its pin and its caches; a per-repo wrapper drifts (#1740).
    assert not (ROOT / "ci" / "act_ci.sh").exists()
    assert "clud_act" not in (ROOT / "bosn.toml").read_text(encoding="utf-8")
