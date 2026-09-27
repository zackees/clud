"""Public promotion must preserve final native assets and the previous pointer."""

from __future__ import annotations

import pytest

from ci.public_release import capture_prior, require_prerelease, require_promoted, require_rollback


def release(tag: str, *, prerelease: bool, asset_id: int = 17) -> dict:
    return {
        "id": 42,
        "tag_name": tag,
        "draft": False,
        "prerelease": prerelease,
        "assets": [{
            "id": asset_id,
            "name": f"clud-{tag}-x86_64-unknown-linux-musl",
            "size": 100,
            "digest": "sha256:" + "a" * 64,
            "state": "uploaded",
            "browser_download_url": f"https://github.com/zackees/clud/releases/download/{tag}/clud-{tag}-x86_64-unknown-linux-musl",
        }],
    }


def test_prior_snapshot_requires_public_latest_stable() -> None:
    prior = {"id": 11, "tag_name": "2.8.14", "prerelease": False, "draft": False}
    catalog = {"channels": {"latest-stable": "2.8.14"}}
    assert capture_prior(prior, catalog, "2.8.15") == {"prior_id": 11, "prior_tag": "2.8.14"}
    with pytest.raises(ValueError, match="canonical"):
        capture_prior(prior, {"channels": {"latest-stable": "2.8.13"}}, "2.8.15")
    with pytest.raises(ValueError, match="newer"):
        capture_prior(prior, catalog, "2.8.13")


def test_promotion_preserves_exact_asset_ids_digests_and_size() -> None:
    name = "clud-2.8.15-x86_64-unknown-linux-musl"
    expected = {name: (100, "a" * 64)}
    candidate = release("2.8.15", prerelease=True)
    snapshot = require_prerelease(candidate, "2.8.15", expected)
    require_promoted(release("2.8.15", prerelease=False), snapshot)
    with pytest.raises(ValueError, match="prerelease"):
        require_prerelease(release("2.8.15", prerelease=False), "2.8.15", expected)
    with pytest.raises(ValueError, match="changed"):
        require_promoted(release("2.8.15", prerelease=False, asset_id=18), snapshot)
    with pytest.raises(ValueError, match="missing"):
        require_prerelease({**candidate, "assets": []}, "2.8.15", expected)


def test_rollback_requires_bad_release_demoted_and_prior_pointer_restored() -> None:
    prior = {"prior_id": 11, "prior_tag": "2.8.14"}
    demoted = release("2.8.15", prerelease=True)
    latest = {"id": 11, "tag_name": "2.8.14"}
    catalog = {"channels": {"latest-stable": "2.8.14"}}
    require_rollback(demoted, latest, catalog, prior)
    with pytest.raises(ValueError, match="rollback"):
        require_rollback(release("2.8.15", prerelease=False), latest, catalog, prior)
    with pytest.raises(ValueError, match="rollback"):
        require_rollback(demoted, {"id": 42, "tag_name": "2.8.15"}, catalog, prior)
