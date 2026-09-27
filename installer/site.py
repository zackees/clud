"""Render the GitHub Pages installer landing page from a validated catalog."""

from __future__ import annotations

import html
import json
from pathlib import Path

from installer.catalog import write_catalog

INSTALLER_SITE_FILES = ("index.html", "install/index.html", "install/manifest.json")
STATIC_ASSETS: dict[str, bytes] = {}


def published_paths() -> tuple[str, ...]:
    """Every installer-owned Pages file that a model-only deploy must retain."""
    return (*INSTALLER_SITE_FILES, *STATIC_ASSETS)

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


def build_site(
    destination: Path,
    releases: list[dict],
    fetch_bytes,
    previous_catalog: dict | None = None,
) -> dict:
    catalog = write_catalog(
        destination / "install" / "manifest.json",
        releases,
        fetch_bytes,
        verified_catalog=previous_catalog,
    )
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
    for relative, body in STATIC_ASSETS.items():
        asset = destination / relative
        asset.parent.mkdir(parents=True, exist_ok=True)
        asset.write_bytes(body)
    return catalog


def main() -> None:
    import argparse

    from installer.catalog import ONLINE_URL, fetch, published_releases

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
    previous_catalog = None
    try:
        previous_catalog = json.loads(fetch(ONLINE_URL))
        print("Found the previous published catalog; unchanged assets will reuse its verification.")
    except (OSError, ValueError):
        print("No previous published catalog is available; verifying release assets from scratch.")
    catalog = build_site(args.destination, releases, fetch, previous_catalog)
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
