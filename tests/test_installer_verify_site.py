from __future__ import annotations

import json

import pytest

from installer import verify_site
from installer.catalog import TARGETS


def write_site(destination, targets: set[tuple[str, str]]) -> None:
    (destination / "install").mkdir(parents=True)
    (destination / "index.html").write_text(
        '<a href="/clud/install/manifest.json"></a>'
        "<script>location.replace('/clud/install/index.html')</script>",
        encoding="utf-8",
    )
    (destination / "install" / "index.html").write_text(
        "Latest stable release: 2.10.0", encoding="utf-8"
    )
    catalog = {
        "channels": {"latest-stable": "2.10.0"},
        "releases": [
            {
                "version": "2.10.0",
                "platforms": [
                    {"platform": {"os": os_name, "arch": arch}}
                    for os_name, arch in sorted(targets)
                ],
            }
        ],
    }
    (destination / "install" / "manifest.json").write_text(
        json.dumps(catalog), encoding="utf-8"
    )


@pytest.fixture(autouse=True)
def latest_release(monkeypatch):
    monkeypatch.setattr(
        verify_site,
        "fetch",
        lambda _url: json.dumps({"tag_name": "2.10.0"}).encode(),
    )


def test_verify_site_accepts_six_targets(tmp_path) -> None:
    targets = {(os_name, arch) for os_name, arch, _ in TARGETS.values()}
    write_site(tmp_path, targets)

    assert verify_site.verify(tmp_path) == "2.10.0"


def test_verify_site_accepts_both_linux_static_variants(tmp_path) -> None:
    targets = {(os_name, arch) for os_name, arch, _ in TARGETS.values()}
    write_site(tmp_path, targets)
    manifest = tmp_path / "install" / "manifest.json"
    catalog = json.loads(manifest.read_text(encoding="utf-8"))
    platforms = catalog["releases"][0]["platforms"]
    for arch in ("x86_64", "aarch64"):
        platforms.append({
            "platform": {"os": "linux", "arch": arch},
            "variant": {"flavor": "static-musl"},
        })
    manifest.write_text(json.dumps(catalog), encoding="utf-8")
    assert verify_site.verify(tmp_path) == "2.10.0"
    platforms.pop()
    manifest.write_text(json.dumps(catalog), encoding="utf-8")
    with pytest.raises(ValueError, match="incomplete static musl"):
        verify_site.verify(tmp_path)


def test_verify_site_rejects_unexpected_target(tmp_path) -> None:
    targets = {(os_name, arch) for os_name, arch, _ in TARGETS.values()}
    targets.add(("unknown", "x86_64"))
    write_site(tmp_path, targets)

    with pytest.raises(ValueError, match="incomplete"):
        verify_site.verify(tmp_path)
