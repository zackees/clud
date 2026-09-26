"""Carry the last published model document into a release-site rebuild."""

from __future__ import annotations

import json
import sys
import urllib.error
from pathlib import Path

from models.manifest import validate_manifest
from models.publish import PAGES, fetch


def preserve(destination: Path) -> bool:
    try:
        body = fetch(f"{PAGES}/models/manifest.json")
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return False
        raise
    document = json.loads(body)
    validate_manifest(document)
    path = destination / "models" / "manifest.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(body)
    return True


if __name__ == "__main__":
    print("preserved" if preserve(Path(sys.argv[1])) else "not yet published")
