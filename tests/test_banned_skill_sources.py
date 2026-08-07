"""Tests for the one-source-of-truth skill lint (#847, DD-039).

Each case is written against the *shape that actually shipped*, not an
invented one: the retired `skill_install.rs` embedded five of its twelve
skills from the root `skills/` tree, and that is what rule 1 must catch.
"""

from pathlib import Path

from ci.banned_skill_sources import (
    ALLOW_MARKER,
    scan_second_source_tree,
    scan_skill_includes,
    scan_skill_writers,
)

ROGUE = Path("rogue_installer.rs")


def test_rule1_catches_a_skill_embedded_from_outside_the_assets_tree() -> None:
    """The exact shape the retired installer used."""
    source = 'content: include_str!("../../../skills/clud-pr/SKILL.md"),'
    assert scan_skill_includes(source)


def test_rule1_allows_the_canonical_assets_tree() -> None:
    source = 'skill_md: include_str!("../assets/skills/clud-pr/SKILL.md"),'
    assert not scan_skill_includes(source)


def test_rule1_allows_a_windows_separator_in_the_canonical_path() -> None:
    source = 'skill_md: include_str!("..\\\\assets\\\\skills\\\\clud-pr\\\\SKILL.md"),'
    assert not scan_skill_includes(source)


def test_rule2_catches_a_new_module_writing_a_skills_path() -> None:
    assert scan_skill_writers(ROGUE, 'let p = home.join(".claude/skills").join(name);')


def test_rule2_catches_the_piecewise_join_form() -> None:
    """`.join(".claude").join("skills")` is the same write, spelled apart."""
    assert scan_skill_writers(ROGUE, 'let p = home.join(".claude").join("skills");')


def test_rule2_exempts_the_one_allowlisted_installer() -> None:
    line = 'let p = home.join(".claude/skills").join(name);'
    assert not scan_skill_writers(Path("skills.rs"), line)


def test_rule2_ignores_unrelated_backend_config_paths() -> None:
    """The false-positive class that made an earlier draft useless.

    An initial version matched any `.claude` / `.codex` literal and produced
    58 hits on settings.json, hooks.json, config.toml and worktree paths —
    none of which install skills. A lint that noisy gets suppressed rather
    than obeyed, so these must stay silent.
    """
    for line in (
        'repo_root.join(".claude").join("settings.json"),',
        'paths.push(home.join(".codex").join("hooks.json"));',
        'let worktrees = temp.path().join(".claude").join("worktrees");',
        'let config = home.join(".codex").join("config.toml");',
    ):
        assert not scan_skill_writers(Path("hook_health.rs"), line), line


def test_the_allow_marker_is_strictly_line_scoped() -> None:
    """Same line only — a marker on the line above must not silence."""
    offending = 'let p = home.join(".claude/skills");'
    assert scan_skill_writers(ROGUE, f"// {ALLOW_MARKER}\n{offending}")
    assert not scan_skill_writers(ROGUE, f"{offending} // {ALLOW_MARKER}")


def test_rule3_reports_a_resurrected_root_skill_tree(tmp_path, monkeypatch) -> None:
    import ci.banned_skill_sources as mod

    monkeypatch.setattr(mod, "ROOT", tmp_path)
    assert scan_second_source_tree() == []

    skill = tmp_path / "skills" / "clud-pr" / "SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("---\nname: clud-pr\n---\n", encoding="utf-8")
    assert scan_second_source_tree() == ["skills/clud-pr/SKILL.md"]


def test_the_repo_itself_has_exactly_one_skill_source_tree() -> None:
    """The invariant, asserted against the real tree rather than a fixture."""
    assert scan_second_source_tree() == []
