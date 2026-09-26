"""Render the GitHub Pages installer landing page from a validated catalog."""

from __future__ import annotations

import html
import json
from pathlib import Path

from installer.catalog import write_catalog

ROOT = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta http-equiv="refresh" content="0;url=/clud/install/index.html">
<title>Install clud</title></head><body>
<a href="/clud/install/manifest.json">Installer manifest</a>
<script>location.replace('/clud/install/index.html')</script>
</body></html>
"""


def render_page(catalog: dict, *, installer_available: bool) -> str:
    latest = html.escape(catalog["channels"]["latest-stable"])
    if installer_available:
        download = (
            '<a href="https://github.com/zackees/clud/releases/latest/download/'
            'clud-installer.exe">Download clud-installer.exe</a>'
        )
        instructions = (
            "Run the downloaded installer and select a version. It asks before writing files."
        )
    else:
        download = (
            '<a href="https://github.com/zackees/clud/releases/latest">'
            "Current release downloads</a> · Universal installer coming in the next release"
        )
        instructions = "Choose a platform download from the current release."
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Install clud</title></head>
<body><main><h1>Install clud</h1>
<p>Latest stable release: {latest}</p>
<p>{download}</p>
<p>{instructions}</p>
<p><a href="manifest.json">View the release catalog</a></p>
</main></body></html>
"""


def build_site(destination: Path, releases: list[dict], fetch_bytes) -> dict:
    catalog = write_catalog(destination / "install" / "manifest.json", releases, fetch_bytes)
    latest = catalog["channels"]["latest-stable"]
    installer_available = any(
        item["tag_name"].removeprefix("v") == latest
        and any(asset["name"] == "clud-installer.exe" for asset in item.get("assets", []))
        for item in releases
    )
    (destination / "index.html").write_text(ROOT, encoding="utf-8")
    (destination / ".nojekyll").write_text("", encoding="utf-8")
    (destination / "install" / "index.html").write_text(
        render_page(catalog, installer_available=installer_available), encoding="utf-8"
    )
    return catalog


def main() -> None:
    import argparse

    from installer.catalog import fetch, published_releases

    parser = argparse.ArgumentParser()
    parser.add_argument("destination", type=Path)
    parser.add_argument(
        "--latest-only",
        action="store_true",
        help="PR smoke check using the currently published latest release",
    )
    args = parser.parse_args()
    releases = published_releases()
    if args.latest_only:
        releases = [
            next(item for item in releases if not item.get("draft") and not item.get("prerelease"))
        ]
    catalog = build_site(args.destination, releases, fetch)
    print(
        json.dumps(
            {
                "latest-stable": catalog["channels"]["latest-stable"],
                "versions": len(catalog["releases"]),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
