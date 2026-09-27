"""The migration lint rejects retired agent aliases and copied deny rules."""

from __future__ import annotations

from ci import banned_legacy_deletion as lint


def test_scan_flags_retired_names_and_hardcoded_claude_rule() -> None:
    names = ["rm" + "-file", "rm" + "-dir", "Bash(" + "rm" + " * )"]
    assert [number for number, _ in lint.scan("\n".join(names))] == [1, 2, 3]


def test_historical_decisions_are_exempt_but_live_docs_are_not() -> None:
    assert not lint.is_scanned("docs/DESIGN_DECISIONS.md")
    assert lint.is_scanned("docs/architecture/rm-tools.md")
    assert lint.is_scanned("crates/clud-bin/src/block_bad_cmd.rs")


def test_repository_is_clean() -> None:
    assert lint.main() == 0
