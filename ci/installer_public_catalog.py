"""Resolve one exact native asset from an anonymous public release catalog."""

from __future__ import annotations

import json
from urllib.request import Request, urlopen

from installer.catalog import ONLINE_URL, VERSION


def catalog_url(mode: str, tag: str) -> str:
    version = tag.removeprefix("v")
    if not VERSION.fullmatch(version) or "-" in version or "+" in version:
        raise ValueError("public release tag must be a stable-form version")
    if mode == "candidate":
        return f"https://github.com/zackees/clud/releases/download/{tag}/installer-candidate-manifest.json"
    if mode == "released":
        return ONLINE_URL
    raise ValueError("invalid public installer mode")


def resolve(mode: str, tag: str, os_name: str, arch: str) -> dict:
    url = catalog_url(mode, tag)
    request = Request(url, headers={"User-Agent": "clud-public-installer-gate"})
    with urlopen(request, timeout=120) as response:
        if not response.geturl().startswith("https://"):
            raise ValueError("public catalog redirected away from HTTPS")
        data = response.read(8 * 1024 * 1024 + 1)
    if len(data) > 8 * 1024 * 1024:
        raise ValueError("public catalog exceeds size limit")
    catalog = json.loads(data)
    version = tag.removeprefix("v")
    channel = "candidate" if mode == "candidate" else "latest-stable"
    if catalog["channels"].get(channel) != version:
        raise ValueError("public catalog channel points to a different version")
    if mode == "candidate" and catalog["channels"].get("latest-stable") == version:
        raise ValueError("candidate incorrectly became latest stable")
    if mode == "released" and "candidate" in catalog["channels"]:
        raise ValueError("canonical catalog exposes a candidate channel")
    release = next(row for row in catalog["releases"] if row["version"] == version)
    matches = [
        row["asset"]
        for row in release["platforms"]
        if row["platform"]["os"] == os_name
        and row["platform"]["arch"] == arch
        and row["asset"]["media_type"] == "application/octet-stream"
        and (os_name != "linux" or row["variant"] == {"flavor": "static-musl"})
    ]
    if len(matches) != 1:
        raise ValueError("public catalog lacks one exact native asset")
    asset = matches[0]
    expected_url = (
        f"https://github.com/zackees/clud/releases/download/{tag}/{asset['filename']}"
    )
    if asset["urls"] != [expected_url]:
        raise ValueError("public native asset URL is not fixed to the release tag")
    if len(asset["sha256"]) != 64 or asset["size_bytes"] <= 0:
        raise ValueError("public native asset has invalid digest or size")
    return {
        "version": version,
        "sha256": asset["sha256"],
        "size_bytes": asset["size_bytes"],
        "url": expected_url,
        "filename": asset["filename"],
    }
