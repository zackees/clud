"""Bind native PR candidate artifacts and host evidence to one source commit."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path
from urllib.request import urlopen

from installer.catalog import ONLINE_URL, SCHEMA, verify_static_musl_elf, version_key
from installer.verify_native_assets import verify_format

TARGETS = {
    "x86_64-pc-windows-msvc": ("windows", "x86_64", "pe", ".exe"),
    "aarch64-pc-windows-msvc": ("windows", "aarch64", "pe", ".exe"),
    "x86_64-apple-darwin": ("darwin", "x86_64", "macho", ""),
    "aarch64-apple-darwin": ("darwin", "aarch64", "macho", ""),
    "x86_64-unknown-linux-musl": ("linux", "x86_64", "elf", ""),
    "aarch64-unknown-linux-musl": ("linux", "aarch64", "elf", ""),
}
REQUIRED_TARGETS = tuple(TARGETS)
HEX_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
HEX_SHA = re.compile(r"[0-9a-f]{40}\Z")
PROVENANCE_FIELDS = {
    "source_sha",
    "target",
    "version",
    "profile",
    "filename",
    "size_bytes",
    "sha256",
}


def candidate_filename(version: str, target: str) -> str:
    if target not in TARGETS:
        raise ValueError(f"unsupported candidate target: {target}")
    return f"clud-{version}-{target}{TARGETS[target][3]}"


def verify_artifact(directory: Path, target: str, source_sha: str, version: str) -> dict:
    """Require exactly one typed binary and its complete build provenance."""
    filename = candidate_filename(version, target)
    if not HEX_SHA.fullmatch(source_sha):
        raise ValueError("expected source SHA must be a full commit ID")
    metadata_path = directory / "provenance.json"
    binary = directory / filename
    if not metadata_path.is_file() or metadata_path.is_symlink():
        raise ValueError("candidate provenance is missing or not a regular file")
    if not binary.is_file() or binary.is_symlink():
        raise ValueError("candidate binary is missing or not a regular file")
    actual = {item.name for item in directory.iterdir()}
    if actual != {filename, "provenance.json"}:
        raise ValueError(f"candidate artifact contains unexpected files: {actual}")
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    if not isinstance(metadata, dict) or set(metadata) != PROVENANCE_FIELDS:
        raise ValueError("candidate provenance fields are incomplete")
    if metadata["source_sha"] != source_sha:
        raise ValueError("candidate source SHA differs from PR head")
    if metadata["target"] != target or metadata["filename"] != filename:
        raise ValueError("candidate target or filename differs from requested target")
    if metadata["version"] != version or metadata["profile"] != "dev":
        raise ValueError("candidate version or build profile differs from PR candidate")
    data = binary.read_bytes()
    if metadata["size_bytes"] != len(data):
        raise ValueError("candidate size differs from provenance")
    digest = hashlib.sha256(data).hexdigest()
    if (
        not isinstance(metadata["sha256"], str)
        or not HEX_SHA256.fullmatch(metadata["sha256"])
        or metadata["sha256"] != digest
    ):
        raise ValueError("candidate digest differs from provenance")
    _os_name, arch, kind, _suffix = TARGETS[target]
    verify_format(data, kind, arch)
    if _os_name == "linux":
        verify_static_musl_elf(data, arch)
    return metadata


def build_fixture(artifacts: Path, output: Path, source_sha: str, version: str) -> dict:
    """Build a strict canonical catalog from all six exact candidate artifacts."""
    platforms = []
    for target in REQUIRED_TARGETS:
        metadata = verify_artifact(artifacts / f"candidate-{target}", target, source_sha, version)
        os_name, arch, _kind, _suffix = TARGETS[target]
        item = {
            "platform": {"os": os_name, "arch": arch},
            "asset": {
                "filename": metadata["filename"],
                "media_type": "application/octet-stream",
                "size_bytes": metadata["size_bytes"],
                "sha256": metadata["sha256"],
                "urls": [
                    f"https://github.com/zackees/clud/releases/download/{version}/{metadata['filename']}"
                ],
                "provides": ["clud"],
            },
        }
        if os_name == "linux":
            item["variant"] = {"flavor": "static-musl"}
        platforms.append(item)
    with urlopen(ONLINE_URL, timeout=20) as response:
        public = json.load(response)
    if (
        public.get("$schema") != SCHEMA
        or public.get("online_url") != ONLINE_URL
        or public.get("kind") != "Catalog"
    ):
        raise ValueError("public prior-release catalog has changed identity")
    prior = next(
        (
            row for row in public["releases"]
            if row["version"] == "2.8.13" and version_key(row["version"]) < version_key(version)
        ),
        None,
    )
    if prior is None or len(prior["platforms"]) != 6:
        raise ValueError("complete 2.8.13 prior release is unavailable")
    catalog = {
        "$schema": SCHEMA,
        "kind": "Catalog",
        "schema_version": 1,
        "tool": "clud",
        "online_url": ONLINE_URL,
        "channels": {"latest-stable": version},
        "releases": [
            {
                "version": version,
                "published_at": "2026-09-27T00:00:00Z",
                "platforms": platforms,
            },
            prior,
        ],
    }
    output.mkdir(parents=True, exist_ok=True)
    (output / "catalog.json").write_text(
        json.dumps(catalog, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    return catalog


def require_evidence(
    rows: list[dict], source_sha: str, version: str, artifacts: Path | None = None
) -> None:
    """Reject omitted, duplicate, skipped, cancelled, or mismatched host lanes."""
    if len(rows) != len(REQUIRED_TARGETS):
        raise ValueError("missing required candidate host evidence")
    seen: set[str] = set()
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError("candidate host evidence is not an object")
        target = row.get("target")
        if target not in TARGETS or target in seen:
            raise ValueError(f"missing or duplicate candidate host evidence: {target}")
        seen.add(target)
        os_name, arch, _kind, _suffix = TARGETS[target]
        if row.get("result") != "success":
            raise ValueError(f"candidate host {target} did not succeed")
        if row.get("source_sha") != source_sha:
            raise ValueError(f"candidate host {target} used another source SHA")
        if row.get("version") != version or row.get("version_output") != f"clud {version}":
            raise ValueError(f"candidate host {target} selected another version")
        if row.get("host_os") != os_name or row.get("host_arch") != arch:
            raise ValueError(f"candidate host {target} ran on another architecture")
        if not isinstance(row.get("resolved_path"), str) or not row["resolved_path"]:
            raise ValueError(f"candidate host {target} did not resolve clud by name")
        digest = row.get("sha256")
        if not isinstance(digest, str) or not HEX_SHA256.fullmatch(digest):
            raise ValueError(f"candidate host {target} lacks a digest")
        if artifacts is not None:
            metadata = verify_artifact(
                artifacts / f"candidate-{target}", target, source_sha, version
            )
            if digest != metadata["sha256"]:
                raise ValueError(f"candidate host {target} executed different bytes")
    if seen != set(REQUIRED_TARGETS):
        raise ValueError("missing required candidate host evidence")


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    artifact = subparsers.add_parser("verify-artifact")
    artifact.add_argument("directory", type=Path)
    artifact.add_argument("target")
    artifact.add_argument("source_sha")
    artifact.add_argument("version")
    aggregate = subparsers.add_parser("aggregate")
    aggregate.add_argument("directory", type=Path)
    aggregate.add_argument("source_sha")
    aggregate.add_argument("version")
    aggregate.add_argument("--artifacts", type=Path)
    fixture = subparsers.add_parser("build-fixture")
    fixture.add_argument("artifacts", type=Path)
    fixture.add_argument("output", type=Path)
    fixture.add_argument("source_sha")
    fixture.add_argument("version")
    args = parser.parse_args()
    if args.command == "verify-artifact":
        print(
            json.dumps(
                verify_artifact(args.directory, args.target, args.source_sha, args.version)
            )
        )
        return
    if args.command == "build-fixture":
        build_fixture(args.artifacts, args.output, args.source_sha, args.version)
        return
    paths = sorted(args.directory.glob("*.json"))
    rows = [json.loads(path.read_text(encoding="utf-8")) for path in paths]
    require_evidence(rows, args.source_sha, args.version, args.artifacts)
    print(f"verified {len(rows)} native candidate host evidence records")


if __name__ == "__main__":
    main()
