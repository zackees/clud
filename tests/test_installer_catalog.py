"""Acceptance checks for the release-backed installer catalog."""

from __future__ import annotations

from io import BytesIO
from zipfile import ZipFile

import pytest

from installer.catalog import catalog_from_releases, executable_member, version_key
from installer.site import build_site
from installer.verify_site import verify


def wheel(executable: str, data: bytes) -> bytes:
    output = BytesIO()
    with ZipFile(output, "w") as archive:
        archive.writestr(f"clud-2.9.0.data/scripts/{executable}", data)
    return output.getvalue()


def release(version: str, url: str, payload: bytes) -> dict:
    import hashlib

    return {
        "tag_name": version,
        "published_at": "2026-09-01T00:00:00Z",
        "assets": [
            {
                "name": f"clud-{version}-py3-none-win_amd64.whl",
                "browser_download_url": url,
                "digest": "sha256:" + hashlib.sha256(payload).hexdigest(),
                "size": len(payload),
            }
        ],
    }


def test_semantic_version_sort_and_prerelease_not_latest() -> None:
    data = wheel("clud.exe", b"MZpayload")
    releases = [
        release("2.9.0", "https://example.com/29", data),
        release("2.10.0-rc.1", "https://example.com/210rc", data),
        release("2.10.0", "https://example.com/210", data),
    ]
    catalog = catalog_from_releases(releases, lambda _: data)
    assert [entry["version"] for entry in catalog["releases"]] == ["2.10.0", "2.10.0-rc.1", "2.9.0"]
    assert catalog["channels"]["latest-stable"] == "2.10.0"
    assert version_key("2.10.0") > version_key("2.9.0")
    assert version_key("2.10.0-rc.10") > version_key("2.10.0-rc.9")
    assert version_key("2.10.0") > version_key("2.10.0-rc.10")


def test_reject_changed_published_bytes() -> None:
    data = wheel("clud.exe", b"MZpayload")
    with pytest.raises(ValueError, match="digest/size mismatch"):
        catalog_from_releases(
            [release("2.9.0", "https://example.com", data)], lambda _: data + b"changed"
        )


def test_reject_wheel_without_real_clud() -> None:
    with pytest.raises(ValueError, match="exactly one"):
        executable_member(wheel("other.exe", b"MZpayload"), "clud.exe")
    with pytest.raises(ValueError, match="not a PE"):
        executable_member(wheel("clud.exe", b"invalid"), "clud.exe")


def test_pages_paths_redirect_and_only_manifest_link(tmp_path) -> None:
    from html.parser import HTMLParser

    data = wheel("clud.exe", b"MZpayload")
    catalog = build_site(
        tmp_path, [release("2.9.0", "https://example.com/29", data)], lambda _: data
    )
    root = (tmp_path / "index.html").read_text(encoding="utf-8")
    page = (tmp_path / "install" / "index.html").read_text(encoding="utf-8")
    public = (tmp_path / "install" / "manifest.json").read_text(encoding="utf-8")

    class Links(HTMLParser):
        def __init__(self):
            super().__init__()
            self.hrefs = []

        def handle_starttag(self, tag, attrs):
            if tag == "a":
                self.hrefs.extend(value for key, value in attrs if key == "href")

    links = Links()
    links.feed(root)
    assert links.hrefs == ["/clud/install/manifest.json"]
    assert "location.replace('/clud/install/index.html')" in root
    assert "2.9.0" in page
    assert "Universal installer coming in the next release" in page
    assert "releases/latest/download/clud-installer.exe" not in page
    assert '"latest-stable": "2.9.0"' in public
    assert catalog["online_url"] == "https://zackees.github.io/clud/install/manifest.json"


def test_direct_binary_wins_over_wheel_for_same_target() -> None:
    import hashlib

    data = wheel("clud.exe", b"MZwheel")
    direct = b"MZdirect"
    item = release("2.9.0", "https://example.com/wheel", data)
    item["assets"].append(
        {
            "name": "clud-2.9.0-x86_64-pc-windows-msvc.exe",
            "browser_download_url": "https://example.com/direct",
            "digest": "sha256:" + hashlib.sha256(direct).hexdigest(),
            "size": len(direct),
        }
    )
    catalog = catalog_from_releases([item], lambda url: direct if url.endswith("direct") else data)
    asset = catalog["releases"][0]["platforms"][0]["asset"]
    assert asset["filename"].endswith("msvc.exe")
    assert asset["sha256"] == hashlib.sha256(direct).hexdigest()


def test_public_site_rejects_incomplete_latest(tmp_path, monkeypatch) -> None:
    import json

    data = wheel("clud.exe", b"MZpayload")
    build_site(tmp_path, [release("2.9.0", "https://example.com/29", data)], lambda _: data)
    monkeypatch.setattr(
        "installer.verify_site.fetch",
        lambda _: json.dumps({"tag_name": "2.9.0"}).encode(),
    )
    with pytest.raises(ValueError, match="incomplete"):
        verify(tmp_path)


def test_page_links_installer_only_when_latest_release_contains_it(tmp_path) -> None:
    data = wheel("clud.exe", b"MZpayload")
    item = release("2.9.0", "https://example.com/29", data)
    item["assets"].append({"name": "clud-installer.exe"})
    build_site(tmp_path, [item], lambda _: data)
    page = (tmp_path / "install" / "index.html").read_text(encoding="utf-8")
    assert "releases/latest/download/clud-installer.exe" in page
    assert "Universal installer coming" not in page
