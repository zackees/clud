"""Verify that generated Pages identify the actual latest GitHub release."""

from __future__ import annotations

import json
import sys
from html.parser import HTMLParser
from pathlib import Path

from installer.catalog import TARGETS, fetch


class Links(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.hrefs: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag == "a":
            self.hrefs.extend(value for key, value in attrs if key == "href" and value)


def verify(site: Path) -> str:
    root = (site / "index.html").read_text(encoding="utf-8")
    page = (site / "install" / "index.html").read_text(encoding="utf-8")
    catalog = json.loads((site / "install" / "manifest.json").read_text(encoding="utf-8"))
    links = Links()
    links.feed(root)
    if links.hrefs != ["/clud/install/manifest.json"]:
        raise ValueError(f"root must link only to catalog: {links.hrefs}")
    if "location.replace('/clud/install/index.html')" not in root:
        raise ValueError("root does not redirect to installer page")
    latest = json.loads(fetch("https://api.github.com/repos/zackees/clud/releases/latest"))
    expected = latest["tag_name"].removeprefix("v")
    if catalog["channels"]["latest-stable"] != expected:
        raise ValueError(f"catalog latest is not published latest: {expected}")
    entry = next((item for item in catalog["releases"] if item["version"] == expected), None)
    if entry is None:
        raise ValueError("latest stable release is absent from the catalog")
    actual_targets = {
        (item["platform"]["os"], item["platform"]["arch"]) for item in entry["platforms"]
    }
    required_targets = {(os_name, arch) for os_name, arch, _ in TARGETS.values()}
    if actual_targets != required_targets or len(entry["platforms"]) != len(required_targets):
        raise ValueError(f"latest stable release is incomplete: {actual_targets}")
    if expected not in page:
        raise ValueError("landing page does not show catalog latest")
    return expected


if __name__ == "__main__":
    print(verify(Path(sys.argv[1])))
