"""Verify and unpack a pinned portable WezTerm Windows bundle for CI."""

from __future__ import annotations

import argparse
import hashlib
import tempfile
import urllib.request
import zipfile
from pathlib import Path

from ci.kitty_wheel import KITTY_SOURCE_REVISION, validate_kitty_bundle

SOURCE_REVISION = f"zackees/wezterm@{KITTY_SOURCE_REVISION}"


def fetch_bundle(url: str, sha256: str, destination: Path) -> Path:
    """Download a ZIP with exact digest and revision, then expose its directory."""
    if not url.startswith("https://"):
        raise ValueError("bundle URL must use HTTPS")
    if len(sha256) != 64 or any(c not in "0123456789abcdef" for c in sha256):
        raise ValueError("bundle SHA-256 must be 64 lowercase hexadecimal characters")
    if destination.exists():
        raise FileExistsError(f"bundle destination already exists: {destination}")

    with tempfile.TemporaryDirectory(
        prefix="clud-kitty-bundle-", dir=destination.parent
    ) as scratch:
        archive = Path(scratch) / "bundle.zip"
        digest = hashlib.sha256()
        with urllib.request.urlopen(url, timeout=60) as response, archive.open("wb") as out:
            while chunk := response.read(1024 * 1024):
                digest.update(chunk)
                out.write(chunk)
        if digest.hexdigest() != sha256:
            raise ValueError("bundle SHA-256 mismatch")

        unpacked = Path(scratch) / "unpacked"
        unpacked.mkdir()
        with zipfile.ZipFile(archive) as zipped:
            for member in zipped.infolist():
                path = Path(member.filename)
                if path.is_absolute() or ".." in path.parts or "\\" in member.filename:
                    raise ValueError(f"bundle contains unsafe path: {member.filename}")
                zipped.extract(member, unpacked)
        actual_revision = (unpacked / "SOURCE_REVISION").read_text(encoding="utf-8").strip()
        if actual_revision != SOURCE_REVISION:
            raise ValueError(f"bundle source revision mismatch: {actual_revision}")
        validate_kitty_bundle(unpacked)
        unpacked.rename(destination)
    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--dest", required=True, type=Path)
    args = parser.parse_args()
    fetch_bundle(args.url, args.sha256, args.dest)


if __name__ == "__main__":
    main()
