"""Publish a versioned catalog for one public prerelease without moving stable."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from urllib.parse import quote

from installer.catalog import ONLINE_URL, REPO_API, candidate_catalog_from_release, fetch


def build(tag: str, destination: Path) -> dict:
    if not tag or "/" in tag or "\\" in tag:
        raise ValueError("invalid release tag")
    release = json.loads(fetch(f"{REPO_API}/tags/{quote(tag, safe='')}"))
    if release.get("tag_name") != tag:
        raise ValueError("release API returned a different tag")
    stable_catalog = json.loads(fetch(ONLINE_URL))
    catalog = candidate_catalog_from_release(release, stable_catalog)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(catalog, indent=2) + "\n", encoding="utf-8")
    return catalog


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    catalog = build(args.tag, args.destination)
    print(f"candidate={catalog['channels']['candidate']} stable={catalog['channels']['latest-stable']}")


if __name__ == "__main__":
    main()
