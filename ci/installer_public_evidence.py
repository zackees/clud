"""Require six exact public host installation records for one release mode."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from ci.installer_candidate import TARGETS
from ci.installer_public_catalog import resolve


def require_evidence(rows: list[dict], expected: dict[str, dict], mode: str, tag: str) -> None:
    if len(rows) != len(TARGETS):
        raise ValueError("missing public host evidence")
    seen = set()
    for row in rows:
        if row.get("mode") != mode or row.get("tag") != tag:
            raise ValueError("public host evidence identifies a different release")
        matching = [
            target
            for target, (os_name, arch, _kind, _suffix) in TARGETS.items()
            if row.get("os") == os_name and row.get("arch") == arch
        ]
        if len(matching) != 1 or matching[0] in seen:
            raise ValueError("duplicate or unknown public host evidence")
        target = matching[0]
        seen.add(target)
        asset = expected[target]
        if (
            row.get("version") != asset["version"]
            or row.get("sha256") != asset["sha256"]
            or row.get("size_bytes") != asset["size_bytes"]
            or row.get("asset_url") != asset["url"]
            or not row.get("resolved_path")
        ):
            raise ValueError(f"public host evidence differs from published asset: {target}")
    if seen != set(TARGETS):
        raise ValueError("missing public host evidence")


def require_guest_evidence(nixos: list[dict], distros: list[dict], expected: dict[str, dict], mode: str, tag: str) -> None:
    if len(nixos) != 2 or {row.get("host_arch") for row in nixos} != {"x86_64", "aarch64"}:
        raise ValueError("missing public NixOS guest evidence")
    for row in nixos:
        target = f"{row['host_arch']}-unknown-linux-musl"
        if (
            row.get("mode") != mode
            or row.get("tag") != tag
            or row.get("version") != tag.removeprefix("v")
            or row.get("sha256") != expected[target]["sha256"]
            or row.get("resolved_path") != "/home/alice/.local/bin/clud"
        ):
            raise ValueError("public NixOS guest evidence differs from published asset")
    if len(distros) != 3 or {row.get("distro") for row in distros} != {"archlinux", "fedora", "alpine"}:
        raise ValueError("missing public distribution guest evidence")
    x64 = expected["x86_64-unknown-linux-musl"]["sha256"]
    for row in distros:
        if (
            row.get("mode") != mode
            or row.get("tag") != tag
            or row.get("version") != tag.removeprefix("v")
            or row.get("host_arch") != "x86_64"
            or row.get("sha256") != x64
            or row.get("resolved_path") != "/home/alice/.local/bin/clud"
        ):
            raise ValueError("public distribution guest evidence differs from published asset")


def log_rows(directory: Path, marker: str) -> list[dict]:
    rows = []
    for path in directory.rglob("*.txt"):
        lines = [line.split(marker, 1)[1].strip() for line in path.read_text(encoding="utf-8").splitlines() if marker in line]
        if len(lines) != 1:
            raise ValueError(f"missing or duplicated guest evidence in {path}")
        if marker == "PUBLIC_NIXOS_EVIDENCE ":
            rows.append(json.loads(lines[0]))
        else:
            rows.append(dict(part.split("=", 1) for part in lines[0].split()))
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("mode", choices=("candidate", "released"))
    parser.add_argument("tag")
    parser.add_argument("--nixos", type=Path, required=True)
    parser.add_argument("--distros", type=Path, required=True)
    args = parser.parse_args()
    files = sorted(args.directory.glob("*.json"))
    rows = [json.loads(path.read_text(encoding="utf-8")) for path in files]
    expected = {
        target: resolve(args.mode, args.tag, os_name, arch)
        for target, (os_name, arch, _kind, _suffix) in TARGETS.items()
    }
    require_evidence(rows, expected, args.mode, args.tag)
    nixos = log_rows(args.nixos, "PUBLIC_NIXOS_EVIDENCE ")
    distros = log_rows(args.distros, "PUBLIC_DISTRO_EVIDENCE ")
    require_guest_evidence(nixos, distros, expected, args.mode, args.tag)
    print(f"PUBLIC_AGGREGATE mode={args.mode} tag={args.tag} hosts={len(rows)} nixos={len(nixos)} distros={len(distros)}")


if __name__ == "__main__":
    main()
