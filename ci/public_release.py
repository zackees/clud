"""Guard prerelease publication, promotion, and rollback with public state checks."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from installer.catalog import ONLINE_URL, version_key
from installer.verify_native_assets import verify as verify_native_assets

REPO = "zackees/clud"
API = f"https://api.github.com/repos/{REPO}"


def capture_prior(latest: dict, catalog: dict, new_tag: str) -> dict:
    if latest.get("draft") or latest.get("prerelease") or not isinstance(latest.get("id"), int):
        raise ValueError("prior latest release is not published stable")
    prior_tag = latest.get("tag_name")
    if not isinstance(prior_tag, str):
        raise ValueError("prior latest release has no tag")
    prior_version = prior_tag.removeprefix("v")
    if catalog.get("channels", {}).get("latest-stable") != prior_version:
        raise ValueError("canonical catalog differs from prior latest release")
    if version_key(new_tag.removeprefix("v")) <= version_key(prior_version):
        raise ValueError("new release must be newer than previous stable")
    return {"prior_id": latest["id"], "prior_tag": prior_tag}


def asset_snapshot(release: dict) -> dict:
    assets = release.get("assets")
    if not isinstance(assets, list) or not assets:
        raise ValueError("release assets are missing")
    result = {}
    for asset in assets:
        name = asset.get("name")
        digest = asset.get("digest")
        if (
            not isinstance(name, str)
            or name in result
            or not isinstance(asset.get("id"), int)
            or not isinstance(asset.get("size"), int)
            or asset["size"] <= 0
            or not isinstance(digest, str)
            or not digest.startswith("sha256:")
            or len(digest) != 71
            or asset.get("state") != "uploaded"
        ):
            raise ValueError("release asset metadata is incomplete or duplicated")
        result[name] = {"id": asset["id"], "size": asset["size"], "digest": digest}
    return result


def require_prerelease(release: dict, tag: str, expected: dict[str, tuple[int, str]]) -> dict:
    if (
        release.get("tag_name") != tag
        or release.get("draft")
        or release.get("prerelease") is not True
    ):
        raise ValueError("release is not the requested public prerelease")
    if not isinstance(release.get("id"), int):
        raise ValueError("public prerelease has no ID")
    assets = asset_snapshot(release)
    for name, (size, digest) in expected.items():
        row = assets.get(name)
        if row is None:
            raise ValueError(f"missing final native release asset: {name}")
        if row["size"] != size or row["digest"] != f"sha256:{digest}":
            raise ValueError(f"published asset differs from final bytes: {name}")
    return {"release_id": release["id"], "tag": tag, "assets": assets}


def require_promoted(release: dict, snapshot: dict) -> None:
    if (
        release.get("id") != snapshot["release_id"]
        or release.get("tag_name") != snapshot["tag"]
        or release.get("draft")
        or release.get("prerelease")
    ):
        raise ValueError("release promotion state is wrong")
    if asset_snapshot(release) != snapshot["assets"]:
        raise ValueError("release assets changed during promotion")


def require_rollback(bad: dict, latest: dict, catalog: dict, prior: dict) -> None:
    if (
        bad.get("prerelease") is not True
        or bad.get("draft")
        or latest.get("id") != prior["prior_id"]
        or latest.get("tag_name") != prior["prior_tag"]
        or catalog.get("channels", {}).get("latest-stable")
        != prior["prior_tag"].removeprefix("v")
    ):
        raise ValueError("rollback did not restore the prior stable pointer")


def api(path: str, *, method: str = "GET", body: dict | None = None) -> dict:
    token = os.environ.get("GH_TOKEN")
    if not token:
        raise ValueError("GH_TOKEN is required for release state changes")
    data = json.dumps(body).encode() if body is not None else None
    request = Request(
        f"{API}/{path}",
        data=data,
        method=method,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
            "User-Agent": "clud-public-release-gate",
        },
    )
    with urlopen(request, timeout=60) as response:
        return json.load(response)


def public_bytes(url: str, limit: int = 200 * 1024 * 1024) -> bytes:
    if not url.startswith("https://"):
        raise ValueError("public release URL must use HTTPS")
    request = Request(url, headers={"User-Agent": "clud-public-release-gate"})
    with urlopen(request, timeout=120) as response:
        if not response.geturl().startswith("https://"):
            raise ValueError("public release redirected away from HTTPS")
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("public release response exceeds size limit")
    return data


def public_catalog() -> tuple[dict, str]:
    data = public_bytes(ONLINE_URL, 8 * 1024 * 1024)
    return json.loads(data), hashlib.sha256(data).hexdigest()


def release_by_tag(tag: str) -> dict:
    return api(f"releases/tags/{tag}")


def load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def save(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def snapshot_command(tag: str, output: Path) -> None:
    try:
        release_by_tag(tag)
    except HTTPError as error:
        if error.code != 404:
            raise
    else:
        raise ValueError("release tag already exists")
    latest = api("releases/latest")
    catalog, digest = public_catalog()
    snapshot = {**capture_prior(latest, catalog, tag), "catalog_sha256": digest}
    save(output, snapshot)
    if path := os.environ.get("GITHUB_OUTPUT"):
        with Path(path).open("a", encoding="utf-8") as stream:
            stream.write(f"prior_tag={snapshot['prior_tag']}\n")
            stream.write(f"prior_id={snapshot['prior_id']}\n")
    print(json.dumps(snapshot, sort_keys=True))


def require_prior_public(snapshot: dict) -> None:
    latest = api("releases/latest")
    catalog, digest = public_catalog()
    if (
        latest.get("id") != snapshot["prior_id"]
        or latest.get("tag_name") != snapshot["prior_tag"]
        or catalog.get("channels", {}).get("latest-stable")
        != snapshot["prior_tag"].removeprefix("v")
        or digest != snapshot["catalog_sha256"]
    ):
        raise ValueError("previous public stable state changed during candidate checks")


def verify_prerelease_command(tag: str, native_dir: Path, prior_path: Path, output: Path) -> None:
    prior = load(prior_path)
    require_prior_public(prior)
    version = tag.removeprefix("v")
    expected = verify_native_assets(native_dir, version)
    release = release_by_tag(tag)
    snapshot = {**prior, **require_prerelease(release, tag, expected)}
    if "installer-candidate-manifest.json" not in snapshot["assets"]:
        raise ValueError("public candidate catalog asset is missing")
    for asset in release["assets"]:
        if asset["name"] not in expected:
            continue
        data = public_bytes(asset["browser_download_url"], expected[asset["name"]][0])
        if (len(data), hashlib.sha256(data).hexdigest()) != expected[asset["name"]]:
            raise ValueError(f"anonymous public asset differs from final bytes: {asset['name']}")
    catalog_url = next(
        asset["browser_download_url"]
        for asset in release["assets"]
        if asset["name"] == "installer-candidate-manifest.json"
    )
    candidate_catalog = json.loads(public_bytes(catalog_url, 8 * 1024 * 1024))
    if (
        candidate_catalog["channels"].get("candidate") != version
        or candidate_catalog["channels"].get("latest-stable")
        != prior["prior_tag"].removeprefix("v")
    ):
        raise ValueError("candidate catalog moved the stable pointer")
    save(output, snapshot)
    print(json.dumps(
        {"tag": tag, "release_id": snapshot["release_id"], "assets": snapshot["assets"]},
        sort_keys=True,
    ))


def promote_command(snapshot: dict) -> None:
    require_prior_public(snapshot)
    release = release_by_tag(snapshot["tag"])
    if release.get("id") != snapshot["release_id"] or release.get("prerelease") is not True:
        raise ValueError("candidate release changed before promotion")
    if asset_snapshot(release) != snapshot["assets"]:
        raise ValueError("candidate assets changed before promotion")
    api(
        f"releases/{snapshot['release_id']}",
        method="PATCH",
        body={"prerelease": False, "make_latest": "true"},
    )
    require_promoted(release_by_tag(snapshot["tag"]), snapshot)
    latest = api("releases/latest")
    if latest.get("id") != snapshot["release_id"]:
        raise ValueError("promoted release is not the latest GitHub release")


def verify_promoted_command(snapshot: dict) -> None:
    require_promoted(release_by_tag(snapshot["tag"]), snapshot)
    latest = api("releases/latest")
    catalog, _digest = public_catalog()
    if (
        latest.get("id") != snapshot["release_id"]
        or catalog.get("channels", {}).get("latest-stable")
        != snapshot["tag"].removeprefix("v")
    ):
        raise ValueError("public stable pointer has not reached the promoted release")


def rollback_command(snapshot: dict) -> None:
    release = release_by_tag(snapshot["tag"])
    if release.get("id") != snapshot["release_id"]:
        raise ValueError("rollback candidate release ID changed")
    latest = api("releases/latest")
    if latest.get("id") not in {snapshot["prior_id"], snapshot["release_id"]}:
        raise ValueError("rollback would overwrite an unrelated latest release")
    if release.get("prerelease") is not True:
        api(
            f"releases/{snapshot['release_id']}",
            method="PATCH",
            body={"prerelease": True, "make_latest": "false"},
        )
    api(f"releases/{snapshot['prior_id']}", method="PATCH", body={"make_latest": "true"})
    if release_by_tag(snapshot["tag"]).get("prerelease") is not True:
        raise ValueError("rollback failed to demote candidate")
    if api("releases/latest").get("id") != snapshot["prior_id"]:
        raise ValueError("rollback failed to restore previous latest release")


def verify_rollback_command(snapshot: dict) -> None:
    bad = release_by_tag(snapshot["tag"])
    latest = api("releases/latest")
    catalog, _digest = public_catalog()
    require_rollback(bad, latest, catalog, snapshot)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    one = sub.add_parser("snapshot")
    one.add_argument("tag")
    one.add_argument("output", type=Path)
    verify = sub.add_parser("verify-prerelease")
    verify.add_argument("tag")
    verify.add_argument("native_dir", type=Path)
    verify.add_argument("prior", type=Path)
    verify.add_argument("output", type=Path)
    for name in ("verify-prior", "promote", "verify-promoted", "rollback", "verify-rollback"):
        command = sub.add_parser(name)
        command.add_argument("snapshot", type=Path)
    args = parser.parse_args()
    if args.command == "snapshot":
        snapshot_command(args.tag, args.output)
    elif args.command == "verify-prerelease":
        verify_prerelease_command(args.tag, args.native_dir, args.prior, args.output)
    elif args.command == "verify-prior":
        require_prior_public(load(args.snapshot))
    elif args.command == "promote":
        promote_command(load(args.snapshot))
    elif args.command == "verify-promoted":
        verify_promoted_command(load(args.snapshot))
    elif args.command == "rollback":
        rollback_command(load(args.snapshot))
    elif args.command == "verify-rollback":
        verify_rollback_command(load(args.snapshot))


if __name__ == "__main__":
    main()
