"""Guard the /grind meta-of-metas rules in the bundled assets (#1412)."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SKILLS = ROOT / "crates/clud-bin/assets/skills"
WORKFLOW = ROOT / "crates/clud-bin/assets/workflows/grind-run.js"


def _read(name: str) -> str:
    return (SKILLS / name / "SKILL.md").read_text(encoding="utf-8")


def test_router_regroups_with_replace_parent() -> None:
    text = _read("grind")
    assert "replace_parent" in text
    assert "## 2b. Regroup" in text


def test_router_keeps_old_body_in_details() -> None:
    text = _read("grind")
    assert "Previous content" in text
    assert "<details>" in text
    assert "no children after regrouping" in text


def test_router_keeps_grind_made_sub_metas() -> None:
    text = _read("grind")
    assert "<!-- grind:v1 -->" in text
    assert "grind:meta" in text


def test_router_records_undo() -> None:
    text = _read("grind")
    assert '"undo"' in text
    assert "reparent" in text
    assert "rewrite" in text


def test_router_nesting_guard() -> None:
    text = _read("grind")
    assert "8 levels" in text
    assert "100" in text


def test_router_one_feature_per_run() -> None:
    text = _read("grind")
    assert "deferred_groups" in text
    assert "ONE feature group" in text


def test_router_no_overlap_is_per_top_meta() -> None:
    text = _read("grind")
    assert "waiting_on_pr" in text
    assert "bugs-only" in text
    assert "different top meta" in text


def test_workflow_enforces_deferral_and_overlap() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")
    assert "deferred_groups" in text
    assert "waiting_on_pr" in text
    assert "no overlap" in text


def test_router_feature_pick_defaults_to_dependency_order() -> None:
    text = _read("grind")
    assert "dependency order" in text
    assert '"(Recommended)"' in text
    assert "Keep as is (simple schedule)" in text


def test_router_no_overlap_covers_sub_meta_feature_prs() -> None:
    """A meta of metas names each feature branch after its sub-meta, so the
    check must look at `T`'s sub-issues, and only at PRs into `<main>`."""
    text = _read("grind")
    assert "--base <main> --search 'head:grind/meta-'" in text
    assert "one of `T`'s sub-issues" in text
    assert "move every feature group into `deferred_groups`" in text


def test_router_attaches_sub_issues_by_rest_id() -> None:
    text = _read("grind")
    assert "gh api repos/<o>/<r>/issues/<n> --jq .id" in text


def test_workflow_defers_children_of_groups_outside_stages() -> None:
    text = WORKFLOW.read_text(encoding="utf-8")
    assert "args.plan.deferred_groups : []).forEach" in text
    assert "const NO_FEATURE_LEFT = BUGS_ONLY ||" in text
