from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_readme_badges_select_canonical_push_runs() -> None:
    readme = (ROOT / "README.md").read_text(encoding="utf-8")

    expected_badges = (
        "[![CI](https://github.com/zackees/clud/actions/workflows/ci.yml/"
        "badge.svg?branch=main&event=push)](https://github.com/zackees/clud/"
        "actions/workflows/ci.yml?query=branch%3Amain+event%3Apush)",
        "[![Auto Release](https://github.com/zackees/clud/actions/workflows/"
        "auto-release.yml/badge.svg?event=push)](https://github.com/zackees/"
        "clud/actions/workflows/auto-release.yml?query=event%3Apush)",
    )

    for badge in expected_badges:
        assert badge in readme
