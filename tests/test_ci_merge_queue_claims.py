"""Guard the merge-queue contract (#2048, #2125).

`main` has a merge queue since the move to talos-hq. A `merge_group` run
selects the routine `minimal` tier; full native coverage stays on the
nightly run, `ci-full` labels and releases. Docs and workflow comments
must not say the queue runs the full matrix, or that no queue exists.
"""

from __future__ import annotations

import re
from pathlib import Path

from ci.ci_matrix import resolve_tier

ROOT = Path(__file__).resolve().parent.parent
FILES = [
    ROOT / ".github" / "workflows" / "ci.yml",
    ROOT / "docs" / "architecture" / "ci.md",
    ROOT / "CLAUDE.md",
]
STALE_CLAIM = re.compile(
    r"no merge queue"
    r"|merge_group(?:`)? (?:trigger )?(?:in `ci\.yml` )?is inert"
    r"|merge(?:_group| queue)\S* (?:runs|selects|gets) (?:the )?(?:`)?full",
    re.IGNORECASE,
)


def test_merge_group_runs_the_minimal_tier() -> None:
    assert resolve_tier("merge_group", "", "") == "minimal"


def test_no_stale_merge_queue_claims() -> None:
    hits = []
    for path in FILES:
        # Collapse comment markers and whitespace so wrapped lines still match.
        text = re.sub(r"\s*#\s*|\s+", " ", path.read_text(encoding="utf-8"))
        hits += [f"{path.name}: {m.group(0)!r}" for m in STALE_CLAIM.finditer(text)]
    assert not hits, (
        "main has a merge queue running the minimal tier (#2125); drop these claims:\n"
        + "\n".join(hits)
    )
