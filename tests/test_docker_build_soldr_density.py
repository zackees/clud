"""Density heuristic + BuildKit cache namespace for the soldr docker tool (#518).

The issue's superseding clarification adds two rules on top of the plain 48 h
threshold covered by `test_docker_build_soldr_gc.py`:

1. A **crowded tag prefix** is evidence of active development churning
   generations, so unreferenced stale generations get a shorter grace — but the
   currently-selected group is never evicted for being in a crowd, and a group a
   container is still pinning keeps the full 48 h.
2. **BuildKit cache** is pruned separately from the named-volume caches, and
   only inside a clud-owned builder, so a sweep never deletes an unrelated
   user's build cache on the same machine.

Both are asserted against pure functions — `gc_plan` and `buildx_prune_args` —
so the safety boundaries hold without a live Docker daemon.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (
    ROOT / "crates" / "clud-bin" / "assets" / "tools" / "docker"
    / "docker_build_soldr.py"
)


@pytest.fixture(scope="module")
def mod():
    spec = importlib.util.spec_from_file_location("docker_build_soldr", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules["docker_build_soldr"] = module
    spec.loader.exec_module(module)
    return module


def _group(key, *, age_hours=1.0, root_exists=True, is_selected=False,
           referenced=False):
    return {
        "project_key": key,
        "age_hours": age_hours,
        "root_exists": root_exists,
        "is_selected": is_selected,
        "referenced": referenced,
    }


def _reasons(pairs):
    return dict(pairs)


# --------------------------------------------------------- density heuristic --


def test_an_uncrowded_prefix_keeps_the_full_48h_grace(mod):
    """Two generations is not a crowd. A 20 h-old group is well past the
    accelerated window but must still be kept, or the heuristic would fire on
    every ordinary two-project machine."""
    groups = [_group("a", age_hours=20), _group("b", age_hours=1)]
    remove, keep = mod.gc_plan(groups)
    assert remove == []
    assert _reasons(keep)["a"] == "within-grace"


def test_a_crowded_prefix_accelerates_unreferenced_generations(mod):
    """Three generations under one prefix is the shape the issue reports (~43 GB
    on the reporting box). Unreferenced ones past the shorter grace go early."""
    groups = [
        _group("selected", age_hours=20, is_selected=True),
        _group("old-a", age_hours=20),
        _group("old-b", age_hours=13),
    ]
    remove, keep = mod.gc_plan(groups)
    assert _reasons(remove) == {
        "old-a": "crowded-prefix-accelerated",
        "old-b": "crowded-prefix-accelerated",
    }
    assert _reasons(keep) == {"selected": "currently-selected"}


def test_the_selected_generation_survives_a_crowded_prefix(mod):
    """Stated explicitly in the issue: the density heuristic accelerates
    cleanup of *older, stale* generations and must never remove the group the
    current invocation would reuse."""
    groups = [
        _group("selected", age_hours=47, is_selected=True),
        _group("x", age_hours=47),
        _group("y", age_hours=47),
        _group("z", age_hours=47),
    ]
    remove, keep = mod.gc_plan(groups)
    assert "selected" not in {k for k, _ in remove}
    assert _reasons(keep) == {"selected": "currently-selected"}


def test_a_referenced_generation_keeps_the_full_threshold_when_crowded(mod):
    """Acceleration applies to *unreferenced* generations. Killing a container
    someone is using is the right trade at 48 h — the issue permits it there —
    but not at 12 h merely because the prefix is busy."""
    groups = [
        _group("live", age_hours=20, referenced=True),
        _group("dead", age_hours=20),
        _group("other", age_hours=1),
    ]
    remove, keep = mod.gc_plan(groups)
    assert _reasons(remove) == {"dead": "crowded-prefix-accelerated"}
    assert _reasons(keep)["live"] == "within-grace"


def test_48h_remains_the_hard_upper_bound_even_when_referenced(mod):
    """The normal threshold is not softened by the new rule: past it a group
    goes, container or no container."""
    groups = [
        _group("ancient", age_hours=49, referenced=True),
        _group("b", age_hours=1),
        _group("c", age_hours=1),
    ]
    remove, _keep = mod.gc_plan(groups)
    assert _reasons(remove) == {"ancient": "stale-past-threshold"}


def test_missing_worktree_still_wins_over_every_grace(mod):
    groups = [
        _group("gone", age_hours=0.0, root_exists=False),
        _group("b", age_hours=1),
        _group("c", age_hours=1),
    ]
    remove, _keep = mod.gc_plan(groups)
    assert _reasons(remove) == {"gone": "worktree-gone"}


def test_density_and_grace_are_configurable(mod):
    """Both knobs are parameters, not constants baked into the branch — the
    issue calls the shorter grace 'configurable'."""
    groups = [_group("a", age_hours=5), _group("b", age_hours=5)]
    remove, _keep = mod.gc_plan(
        groups, density_threshold=2, crowded_grace_hours=4.0)
    assert _reasons(remove) == {
        "a": "crowded-prefix-accelerated",
        "b": "crowded-prefix-accelerated",
    }


def test_a_group_absent_referenced_key_defaults_to_unreferenced(mod):
    """`_discover_managed_groups` may not populate `referenced`. Defaulting to
    'not referenced' keeps the sweep working rather than silently pinning every
    group forever — the age gates still bound what it can touch."""
    groups = [
        {"project_key": "a", "age_hours": 20, "root_exists": True,
         "is_selected": False},
        {"project_key": "b", "age_hours": 1, "root_exists": True,
         "is_selected": False},
        {"project_key": "c", "age_hours": 1, "root_exists": True,
         "is_selected": False},
    ]
    remove, _keep = mod.gc_plan(groups)
    assert _reasons(remove) == {"a": "crowded-prefix-accelerated"}


# ------------------------------------------------------ buildkit namespacing --


def test_buildkit_prune_targets_cluds_builder_not_the_default(mod):
    """The load-bearing guarantee: a clud sweep must never delete an unrelated
    user's BuildKit cache. Scoping is by `--builder`, asserted on the argv."""
    args = mod.buildx_prune_args(48.0, force=True)
    assert "--builder" in args
    assert args[args.index("--builder") + 1] == mod.BUILDER_NAME
    assert "clud" in mod.BUILDER_NAME
    assert "default" not in args


def test_buildkit_prune_never_touches_records_younger_than_the_threshold(mod):
    args = mod.buildx_prune_args(48.0, force=True)
    assert "--filter" in args
    assert args[args.index("--filter") + 1] == "until=48h"


def test_buildkit_prune_is_dry_run_unless_forced(mod):
    """`gc` is dry-run by default and the cache prune must follow the same rule
    — a bare `gc` that silently deleted build cache would be a nasty surprise."""
    assert "--force" not in mod.buildx_prune_args(48.0, force=False)
    assert "--force" in mod.buildx_prune_args(48.0, force=True)


def test_buildkit_prune_honors_a_custom_threshold(mod):
    assert "until=12h" in mod.buildx_prune_args(12.0, force=False)
    assert "until=0.5h" in mod.buildx_prune_args(0.5, force=False)
