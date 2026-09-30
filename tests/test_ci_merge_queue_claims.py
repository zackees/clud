"""Guard against docs claiming a merge queue that is not configured (#1651).

zackees/clud has no merge-queue ruleset and no branch protection on `main`
(verified 2026-09-30), and no `merge_group` run has ever happened. Docs and
workflow comments must not say the queue runs or backstops anything. If a
queue is enabled later, update this test together with the docs.
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILES = [
    ROOT / ".github" / "workflows" / "ci.yml",
    ROOT / "docs" / "architecture" / "ci.md",
    ROOT / "CLAUDE.md",
]
FALSE_CLAIM = re.compile(
    r"merge queue (?:always|still|runs|is always|requires)"
    r"|ci-full, merge queue"
    r"|ci-full, merge_group, and",
    re.IGNORECASE,
)


def test_no_unbacked_merge_queue_claims() -> None:
    hits = []
    for path in FILES:
        # Collapse comment markers and whitespace so wrapped lines still match.
        text = re.sub(r"\s*#\s*|\s+", " ", path.read_text(encoding="utf-8"))
        hits += [f"{path.name}: {m.group(0)!r}" for m in FALSE_CLAIM.finditer(text)]
    assert not hits, (
        "no merge queue is configured for main (#1651); drop these claims:\n"
        + "\n".join(hits)
    )
