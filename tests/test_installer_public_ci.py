"""Install the anonymous public release bytes on each native host."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import sys
from pathlib import Path
from urllib.request import Request, urlopen

import pytest

from ci.installer_candidate import TARGETS
from ci.installer_public_evidence import require_evidence, require_guest_evidence
from installer.catalog import ONLINE_URL, verify_direct_executable
from tests.process import run as run_process
from tests.test_installer_native_entry import installer_target  # noqa: F401


@pytest.mark.parametrize("os_name", ["darwin", "windows"])
def test_public_executable_flavor_accepts_catalog_rows_without_variant(os_name: str) -> None:
    row = {
        "platform": {"os": os_name},
        "asset": {"media_type": "application/octet-stream"},
    }
    assert public_executable_flavor(row) is None


def test_public_executable_flavor_preserves_linux_static_musl() -> None:
    assert public_executable_flavor({"variant": {"flavor": "static-musl"}}) == "static-musl"


def public_executable_flavor(row: dict) -> str | None:
    return row.get("variant", {}).get("flavor")


def public_bytes(url: str, limit: int = 200 * 1024 * 1024) -> bytes:
    if not url.startswith("https://"):
        raise ValueError("public installer URL must use HTTPS")
    request = Request(url, headers={"User-Agent": "clud-public-installer-gate"})
    with urlopen(request, timeout=120) as response:
        if not response.geturl().startswith("https://"):
            raise ValueError("public installer redirected away from HTTPS")
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("public installer response exceeds size limit")
    return data


def test_public_aggregate_rejects_missing_and_changed_host_bytes() -> None:
    tag = "2.10.0"
    expected = {}
    rows = []
    for target, (os_name, arch, _kind, _suffix) in TARGETS.items():
        url = f"https://github.com/zackees/clud/releases/download/{tag}/clud-{tag}-{target}"
        expected[target] = {"version": tag, "sha256": "a" * 64, "size_bytes": 100, "url": url}
        rows.append({
            "mode": "candidate",
            "tag": tag,
            "version": tag,
            "os": os_name,
            "arch": arch,
            "sha256": "a" * 64,
            "size_bytes": 100,
            "asset_url": url,
            "resolved_path": "/user/bin/clud",
        })
    require_evidence(rows, expected, "candidate", tag)
    with pytest.raises(ValueError, match="missing"):
        require_evidence(rows[:-1], expected, "candidate", tag)
    with pytest.raises(ValueError, match="differs"):
        require_evidence([*rows[:-1], {**rows[-1], "sha256": "b" * 64}], expected, "candidate", tag)

    nixos = [
        {
            "mode": "candidate", "tag": tag, "version": tag, "host_arch": arch,
            "sha256": "a" * 64, "resolved_path": "/home/alice/.local/bin/clud",
        }
        for arch in ("x86_64", "aarch64")
    ]
    distros = [
        {
            "mode": "candidate", "tag": tag, "version": tag,
            "host_arch": "x86_64", "distro": distro, "sha256": "a" * 64,
            "resolved_path": "/home/alice/.local/bin/clud",
        }
        for distro in ("archlinux", "fedora", "alpine")
    ]
    require_guest_evidence(nixos, distros, expected, "candidate", tag)
    with pytest.raises(ValueError, match="missing"):
        require_guest_evidence(nixos[:-1], distros, expected, "candidate", tag)
    with pytest.raises(ValueError, match="differs"):
        require_guest_evidence(
            nixos,
            [*distros[:-1], {**distros[-1], "sha256": "b" * 64}],
            expected,
            "candidate",
            tag,
        )


def test_public_release_installs_by_name(installer_target, tmp_path: Path) -> None:  # noqa: F811
    tag = os.environ.get("CLUD_PUBLIC_RELEASE_TAG")
    if not tag:
        pytest.skip("public release tag is provided by the release workflow")
    mode = os.environ["CLUD_PUBLIC_MODE"]
    if mode not in {"candidate", "released"}:
        raise ValueError("invalid public release mode")
    version = tag.removeprefix("v")
    catalog_url = (
        f"https://github.com/zackees/clud/releases/download/{tag}/installer-candidate-manifest.json"
        if mode == "candidate"
        else ONLINE_URL
    )
    catalog = json.loads(public_bytes(catalog_url, 8 * 1024 * 1024))
    channel = "candidate" if mode == "candidate" else "latest-stable"
    assert catalog["channels"][channel] == version
    if mode == "candidate":
        assert catalog["channels"]["latest-stable"] != version
    else:
        assert "candidate" not in catalog["channels"]

    os_name = (
        "windows" if sys.platform == "win32" else "darwin" if sys.platform == "darwin" else "linux"
    )
    arch = platform.machine().lower()
    arch = {"amd64": "x86_64", "arm64": "aarch64"}.get(arch, arch)
    release = next(row for row in catalog["releases"] if row["version"] == version)
    matches = [
        row
        for row in release["platforms"]
        if row["platform"]["os"] == os_name
        and row["platform"]["arch"] == arch
        and row["asset"]["media_type"] == "application/octet-stream"
        and (os_name != "linux" or row["variant"] == {"flavor": "static-musl"})
    ]
    assert len(matches) == 1, matches
    asset = matches[0]["asset"]
    payload = public_bytes(asset["urls"][0])
    digest = hashlib.sha256(payload).hexdigest()
    assert len(payload) == asset["size_bytes"]
    assert digest == asset["sha256"]
    verify_direct_executable(payload, os_name, arch, public_executable_flavor(matches[0]))
    binary = tmp_path / asset["filename"]
    binary.write_bytes(payload)
    binary.chmod(0o755)

    env, destination = installer_target
    if os_name == "darwin":
        env["SHELL"] = "/bin/zsh"
    if mode == "candidate":
        env["CLUD_INSTALLER_CANDIDATE_TAG"] = tag
    else:
        env.pop("CLUD_INSTALLER_CANDIDATE_TAG", None)
    source = run_process(
        [str(binary), "--version"], env=env, capture_output=True, timeout=15, check=False
    )
    assert source.returncode == 0
    assert source.stdout.strip() == f"clud {version}".encode()
    result = run_process(
        [str(binary), "--installer", "--install-version", version, "--yes"],
        env=env,
        capture_output=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    assert hashlib.sha256(destination.read_bytes()).hexdigest() == digest

    if os_name == "windows":
        script = (
            '$u=[Environment]::GetEnvironmentVariable("Path","User"); '
            '$m=[Environment]::GetEnvironmentVariable("Path","Machine"); '
            '$env:Path="$u;$m"; '
            '$c=Get-Command clud -CommandType Application -ErrorAction Stop; '
            'Write-Output $c.Source; Write-Output (& $c.Source --version)'
        )
        lookup = run_process(
            ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
            env=env,
            capture_output=True,
            timeout=25,
            check=False,
        )
    else:
        shell = "/bin/zsh" if os_name == "darwin" else "/bin/bash"
        fresh_env = {**env, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}
        fresh_env.pop("BASH_ENV", None)
        lookup = run_process(
            [shell, "-l", "-c", "command -v clud; clud --version"],
            env=fresh_env,
            capture_output=True,
            timeout=25,
            check=False,
        )
    assert lookup.returncode == 0, lookup.stderr.decode(errors="replace")
    lines = lookup.stdout.decode(errors="replace").strip().splitlines()
    assert lines == [str(destination), f"clud {version}"]
    evidence = {
        "mode": mode,
        "tag": tag,
        "version": version,
        "os": os_name,
        "arch": arch,
        "sha256": digest,
        "size_bytes": len(payload),
        "asset_url": asset["urls"][0],
        "resolved_path": lines[0],
    }
    output = Path(os.environ["CLUD_PUBLIC_EVIDENCE"])
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(evidence, sort_keys=True) + "\n", encoding="utf-8")
    print("PUBLIC_EVIDENCE " + json.dumps(evidence, sort_keys=True))
