"""Build the public clud catalog from published GitHub release assets.

The generator deliberately does not infer an installable target from a wheel
name alone.  It opens each wheel and checks that the one clud executable that
the installer would extract is present and has executable bytes.
"""

from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path
from urllib.request import Request, urlopen
from zipfile import ZipFile

SCHEMA = "https://zackees.github.io/manifest.json/v1/manifest.schema.json"
ONLINE_URL = "https://zackees.github.io/clud/install/manifest.json"
REPO_API = "https://api.github.com/repos/zackees/clud/releases"
TARGETS = {
    "win_amd64": ("windows", "x86_64", "clud.exe"),
    "win_arm64": ("windows", "aarch64", "clud.exe"),
    "macosx_10_15_x86_64": ("darwin", "x86_64", "clud"),
    "macosx_11_0_arm64": ("darwin", "aarch64", "clud"),
    "manylinux_2_17_x86_64.manylinux2014_x86_64": ("linux", "x86_64", "clud"),
    "manylinux_2_17_aarch64.manylinux2014_aarch64": ("linux", "aarch64", "clud"),
}
DIRECT_TARGETS = {
    "x86_64-pc-windows-msvc.exe": ("windows", "x86_64", "clud.exe"),
    "aarch64-pc-windows-msvc.exe": ("windows", "aarch64", "clud.exe"),
    "x86_64-apple-darwin": ("darwin", "x86_64", "clud"),
    "aarch64-apple-darwin": ("darwin", "aarch64", "clud"),
    "x86_64-unknown-linux-gnu": ("linux", "x86_64", "clud"),
    "aarch64-unknown-linux-gnu": ("linux", "aarch64", "clud"),
}
VERSION = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?$")


def version_key(version: str) -> tuple:
    match = re.match(r"^(\d+)\.(\d+)\.(\d+)(.*)$", version)
    if not match:
        raise ValueError(f"invalid version: {version}")
    major, minor, patch, suffix = match.groups()
    prerelease = suffix.split("+", 1)[0].removeprefix("-") if suffix.startswith("-") else ""
    identifiers = tuple(
        (0, int(part)) if part.isdecimal() else (1, part) for part in prerelease.split(".") if part
    )
    return int(major), int(minor), int(patch), int(not prerelease), identifiers


def wheel_target(name: str, version: str) -> tuple[str, str, str] | None:
    prefix = f"clud-{version}-py3-none-"
    if not name.startswith(prefix) or not name.endswith(".whl"):
        return None
    return TARGETS.get(name[len(prefix) : -4])


def direct_target(name: str, version: str) -> tuple[str, str, str] | None:
    prefix = f"clud-{version}-"
    return DIRECT_TARGETS.get(name[len(prefix) :]) if name.startswith(prefix) else None


def is_executable(data: bytes, executable: str) -> bool:
    return (
        data.startswith(b"MZ")
        if executable.endswith(".exe")
        else (data.startswith(b"\x7fELF") or data.startswith(b"\xcf\xfa\xed\xfe"))
    )


def executable_member(wheel_bytes: bytes, executable: str) -> str:
    from io import BytesIO

    with ZipFile(BytesIO(wheel_bytes)) as wheel:
        members = [
            name for name in wheel.namelist() if name.endswith(f".data/scripts/{executable}")
        ]
        if len(members) != 1:
            raise ValueError(f"wheel must contain exactly one {executable}: {members}")
        data = wheel.read(members[0])
        if not is_executable(data, executable):
            raise ValueError("clud is not a PE, ELF or Mach-O executable")
        return members[0]


def _verified_catalog_assets(
    catalog: dict | None,
) -> dict[tuple[str, str], tuple[tuple[str, str], dict]]:
    if (
        not isinstance(catalog, dict)
        or catalog.get("kind") != "Catalog"
        or catalog.get("schema_version") != 1
        or catalog.get("tool") != "clud"
        or catalog.get("online_url") != ONLINE_URL
    ):
        return {}
    verified = {}
    entries = catalog.get("releases")
    if not isinstance(entries, list):
        return {}
    for release in entries:
        if not isinstance(release, dict) or not isinstance(release.get("version"), str):
            continue
        platforms = release.get("platforms")
        if not isinstance(platforms, list):
            continue
        for item in platforms:
            if not isinstance(item, dict):
                continue
            platform = item.get("platform")
            asset = item.get("asset")
            if (
                isinstance(platform, dict)
                and isinstance(platform.get("os"), str)
                and isinstance(platform.get("arch"), str)
                and isinstance(asset, dict)
                and isinstance(asset.get("filename"), str)
            ):
                verified[(release["version"], asset["filename"])] = (
                    (platform["os"], platform["arch"]),
                    asset,
                )
    return verified


def catalog_from_releases(
    releases: list[dict], fetch_bytes, verified_catalog: dict | None = None
) -> dict:
    cached_assets = _verified_catalog_assets(verified_catalog)
    entries = []
    for release in releases:
        version = release["tag_name"].removeprefix("v")
        if release.get("draft") or not VERSION.fullmatch(version):
            continue
        platforms = []
        seen = set()
        assets = sorted(
            release.get("assets", []),
            key=lambda asset: direct_target(asset["name"], version) is None,
        )
        for asset in assets:
            direct = direct_target(asset["name"], version)
            target = direct or wheel_target(asset["name"], version)
            if target is None:
                continue
            os_name, arch, executable = target
            if (os_name, arch) in seen:
                if direct:
                    raise ValueError(f"duplicate direct target in {version}: {target}")
                continue
            seen.add((os_name, arch))
            filename = asset["name"]
            url = asset["browser_download_url"]
            api_digest = asset.get("digest")
            size = asset.get("size")
            cached = cached_assets.get((version, filename))
            cached_platform, cached_asset = cached if cached else (None, None)
            media_type = "application/octet-stream" if direct else "application/zip"
            reused = (
                cached_platform == (os_name, arch)
                and cached_asset.get("filename") == filename
                and cached_asset.get("media_type") == media_type
                and cached_asset.get("size_bytes") == size
                and isinstance(cached_asset.get("sha256"), str)
                and len(cached_asset["sha256"]) == 64
                and all(c in "0123456789abcdef" for c in cached_asset["sha256"])
                and api_digest == f"sha256:{cached_asset.get('sha256')}"
                and cached_asset.get("urls") == [url]
                and cached_asset.get("provides") == ["clud"]
                and isinstance(size, int)
                and isinstance(api_digest, str)
            )
            if reused:
                digest = cached_asset["sha256"]
                print(f"reused verified release asset: {filename} ({size} bytes)", flush=True)
            else:
                data = fetch_bytes(url)
                digest = hashlib.sha256(data).hexdigest()
                if api_digest != f"sha256:{digest}" or size != len(data):
                    raise ValueError(f"published wheel digest/size mismatch: {filename}")
                if direct:
                    if not is_executable(data, executable):
                        raise ValueError(f"published asset is not clud: {filename}")
                else:
                    executable_member(data, executable)
                print(f"verified release asset: {filename} ({len(data)} bytes)", flush=True)
            platforms.append(
                {
                    "platform": {"os": os_name, "arch": arch},
                    "asset": {
                        "filename": filename,
                        "media_type": media_type,
                        "size_bytes": size,
                        "sha256": digest,
                        "urls": [url],
                        "provides": ["clud"],
                    },
                }
            )
        if platforms:
            entries.append(
                {
                    "version": version,
                    "published_at": release["published_at"],
                    "platforms": sorted(
                        platforms,
                        key=lambda item: (item["platform"]["os"], item["platform"]["arch"]),
                    ),
                }
            )
    entries.sort(key=lambda entry: version_key(entry["version"]), reverse=True)
    stable = next((entry["version"] for entry in entries if "-" not in entry["version"]), None)
    if not stable:
        raise ValueError("no installable stable release")
    return {
        "$schema": SCHEMA,
        "kind": "Catalog",
        "schema_version": 1,
        "tool": "clud",
        "online_url": ONLINE_URL,
        "channels": {"latest-stable": stable},
        "releases": entries,
    }


def fetch(url: str) -> bytes:
    request = Request(
        url,
        headers={"Accept": "application/vnd.github+json", "User-Agent": "clud-catalog"},
    )
    with urlopen(request, timeout=60) as response:
        return response.read()


def published_releases(fetch_bytes=fetch) -> list[dict]:
    releases = []
    page = 1
    while True:
        batch = json.loads(fetch_bytes(f"{REPO_API}?per_page=100&page={page}"))
        releases.extend(batch)
        if len(batch) < 100:
            return releases
        page += 1


def write_catalog(
    destination: Path,
    releases: list[dict],
    fetch_bytes=fetch,
    verified_catalog: dict | None = None,
) -> dict:
    catalog = catalog_from_releases(releases, fetch_bytes, verified_catalog)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(catalog, indent=2) + "\n", encoding="utf-8")
    return catalog
