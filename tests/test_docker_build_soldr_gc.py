"""Unit tests for the bundled soldr docker-build tool's GC decision logic
(issue #518).

`gc_plan` is a pure function, so the safe-deletion boundaries — never evict the
selected group, immediate eviction of groups whose worktree is gone, the 48h
threshold, and the crowded-prefix protection of the active group — are pinned
here without a live Docker daemon. Mirrors the importlib load pattern in
`test_docker_recover.py`.
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


@pytest.fixture
def mod():
    name = "clud_test_docker_build_soldr"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


def _group(key, *, age_hours=1.0, root_exists=True, is_selected=False):
    return {
        "project_key": key,
        "age_hours": age_hours,
        "root_exists": root_exists,
        "is_selected": is_selected,
    }


def _keys(pairs):
    return {k for k, _ in pairs}


def test_stale_group_past_threshold_is_removed(mod):
    remove, keep = mod.gc_plan([_group("old", age_hours=49)])
    assert _keys(remove) == {"old"}
    assert keep == []


def test_group_within_grace_is_kept(mod):
    remove, keep = mod.gc_plan([_group("fresh", age_hours=1)])
    assert remove == []
    assert _keys(keep) == {"fresh"}


def test_threshold_is_inclusive(mod):
    remove, _ = mod.gc_plan([_group("edge", age_hours=48.0)])
    assert _keys(remove) == {"edge"}


def test_missing_worktree_is_removed_regardless_of_age(mod):
    remove, keep = mod.gc_plan([_group("gone", age_hours=0.1, root_exists=False)])
    assert _keys(remove) == {"gone"}
    assert keep == []


def test_selected_group_is_never_removed_even_when_stale(mod):
    # The currently-selected/reused group must survive even past the threshold.
    remove, keep = mod.gc_plan(
        [_group("active", age_hours=1000, is_selected=True)]
    )
    assert remove == []
    assert _keys(keep) == {"active"}


def test_selected_group_survives_even_with_missing_root_guard_order(mod):
    # is_selected wins over every other rule (protects an active checkout whose
    # resolve() momentarily differs, etc.).
    remove, _ = mod.gc_plan(
        [_group("active", age_hours=1000, root_exists=False, is_selected=True)]
    )
    assert remove == []


def test_mixed_set_partitions_correctly(mod):
    groups = [
        _group("selected", age_hours=1000, is_selected=True),
        _group("stale", age_hours=72),
        _group("gone", age_hours=2, root_exists=False),
        _group("fresh", age_hours=3),
    ]
    remove, keep = mod.gc_plan(groups)
    assert _keys(remove) == {"stale", "gone"}
    assert _keys(keep) == {"selected", "fresh"}


def test_custom_threshold_is_honored(mod):
    groups = [_group("g", age_hours=10)]
    assert mod.gc_plan(groups, threshold_hours=48)[0] == []
    assert _keys(mod.gc_plan(groups, threshold_hours=6)[0]) == {"g"}


def test_created_at_parse_and_label_line(mod):
    # Docker's CreatedAt with a tz name still parses to an epoch.
    epoch = mod._parse_docker_created_at("2026-07-28 11:04:46 -0700 PDT")
    assert epoch is not None
    assert epoch > 0
    assert mod._parse_docker_created_at("garbage") is None

    key, root, created = mod._parse_managed_line(
        "container",
        "com.clud.docker-build.project-key=abc123,"
        "com.clud.docker-build.project-root=/repo\t2026-07-28 11:04:46 -0700 PDT",
    )
    assert key == "abc123"
    assert root == "/repo"
    assert created is not None

    vkey, vroot, vcreated = mod._parse_managed_line(
        "volume", "clud-docker-build-soldr-abc123-target"
    )
    assert vkey == "abc123"
    assert (vroot, vcreated) == (None, None)


def test_label_args_identify_managed_resource(mod, tmp_path):
    args = mod._label_args(tmp_path, "container")
    joined = " ".join(args)
    assert "com.clud.docker-build.managed=true" in joined
    assert "com.clud.docker-build.role=container" in joined
    assert "com.clud.docker-build.stack=soldr" in joined


def test_legacy_unlabelled_volume_names_still_resolve_to_a_group(mod):
    """#518: volumes created before labelling carry no labels at all. Discovery
    must still recognise them by their `clud-docker-build-soldr-<key>-<role>`
    name, or the abandoned cache sets that motivated the issue stay invisible
    to gc forever."""
    key, root, created = mod._parse_managed_line(
        "volume", "clud-docker-build-soldr-0c1f4b2c0e0a-target"
    )
    assert key == "0c1f4b2c0e0a"
    assert (root, created) == (None, None)


def test_unrelated_volumes_are_never_matched(mod):
    """The name-prefix fallback must not widen the blast radius: the issue
    calls out an unrelated 89GB `soldr-perf-target` volume that clud must never
    sweep."""
    for name in (
        "soldr-perf-target",
        "soldr-perf-target-soldr2-e27990ba",
        "some-other-vol",
        "clud-docker-build-python-abc123-target",  # different stack
    ):
        assert mod._parse_managed_line("volume", name)[0] is None, name


def test_rfc3339_volume_timestamps_parse(mod):
    """`docker volume inspect` emits RFC3339, unlike `docker ps`. Without this
    a legacy (containerless) group has no age, reads as brand new, and is kept
    forever — the exact failure this fix targets."""
    assert mod._parse_docker_created_at("2026-07-28T11:04:46-07:00") is not None
    assert mod._parse_docker_created_at("2026-07-28T11:04:46Z") is not None
    assert mod._parse_docker_created_at("2026-07-28 11:04:46 -0700 PDT") is not None
