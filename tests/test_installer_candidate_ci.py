"""A PR candidate must keep its source and six native-host results together."""

from __future__ import annotations

import hashlib
import io
import json
import os
import platform
import sys
from pathlib import Path

import pytest

from ci.installer_candidate import (
    REQUIRED_TARGETS,
    TARGETS,
    build_fixture,
    candidate_filename,
    require_evidence,
    verify_artifact,
)
from tests.process import run as run_process
from tests.test_installer_native_entry import installer_target  # noqa: F401
from tests.test_installer_verify_native_assets import native_payload

SOURCE_SHA = "a" * 40
VERSION = "2.8.14"
TARGET = "x86_64-unknown-linux-musl"


def artifact(tmp_path, *, target=TARGET, source_sha=SOURCE_SHA, payload=None):
    payload = payload or native_payload("elf", "x86_64")
    filename = f"clud-{VERSION}-{target}"
    (tmp_path / filename).write_bytes(payload)
    metadata = {
        "source_sha": source_sha,
        "target": target,
        "version": VERSION,
        "profile": "dev",
        "filename": filename,
        "size_bytes": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    }
    (tmp_path / "provenance.json").write_text(
        json.dumps(metadata), encoding="utf-8"
    )
    return metadata


def test_candidate_artifact_is_bound_to_pr_head_and_native_format(tmp_path) -> None:
    metadata = artifact(tmp_path)
    assert verify_artifact(tmp_path, TARGET, SOURCE_SHA, VERSION) == metadata

    (tmp_path / metadata["filename"]).write_bytes(b"changed")
    with pytest.raises(ValueError, match=r"digest|size|format"):
        verify_artifact(tmp_path, TARGET, SOURCE_SHA, VERSION)


def test_candidate_rejects_other_source_or_architecture(tmp_path) -> None:
    artifact(tmp_path, source_sha="b" * 40)
    with pytest.raises(ValueError, match="source"):
        verify_artifact(tmp_path, TARGET, SOURCE_SHA, VERSION)

    artifact(tmp_path, payload=native_payload("elf", "aarch64"))
    with pytest.raises(ValueError, match="architecture"):
        verify_artifact(tmp_path, TARGET, SOURCE_SHA, VERSION)


def evidence(target: str) -> dict:
    return {
        "target": target,
        "source_sha": SOURCE_SHA,
        "version": VERSION,
        "sha256": "c" * 64,
        "host_os": (
            "windows" if "windows" in target else "darwin" if "darwin" in target else "linux"
        ),
        "host_arch": "aarch64" if target.startswith("aarch64") else "x86_64",
        "resolved_path": "/user/bin/clud",
        "version_output": f"clud {VERSION}",
        "result": "success",
    }


def test_aggregate_rejects_missing_skipped_and_mismatched_host_evidence() -> None:
    rows = [evidence(target) for target in REQUIRED_TARGETS]
    require_evidence(rows, SOURCE_SHA, VERSION)
    with pytest.raises(ValueError, match="missing"):
        require_evidence(rows[:-1], SOURCE_SHA, VERSION)
    with pytest.raises(ValueError, match="did not succeed"):
        require_evidence(
            [*rows[:-1], {**rows[-1], "result": "skipped"}], SOURCE_SHA, VERSION
        )
    with pytest.raises(ValueError, match="source"):
        require_evidence(
            [*rows[:-1], {**rows[-1], "source_sha": "b" * 40}], SOURCE_SHA, VERSION
        )


def test_fixture_binds_all_six_artifacts_and_prior_release(tmp_path, monkeypatch) -> None:
    artifacts = tmp_path / "artifacts"
    rows = []
    for target in REQUIRED_TARGETS:
        directory = artifacts / f"candidate-{target}"
        directory.mkdir(parents=True)
        _os_name, arch, kind, _suffix = TARGETS[target]
        payload = native_payload(kind, arch)
        metadata = {
            "source_sha": SOURCE_SHA,
            "target": target,
            "version": VERSION,
            "profile": "dev",
            "filename": candidate_filename(VERSION, target),
            "size_bytes": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(),
        }
        (directory / metadata["filename"]).write_bytes(payload)
        (directory / "provenance.json").write_text(json.dumps(metadata), encoding="utf-8")
        rows.append({**evidence(target), "sha256": metadata["sha256"]})
    prior = {
        "$schema": "https://zackees.github.io/manifest.json/v1/manifest.schema.json",
        "online_url": "https://zackees.github.io/clud/install/manifest.json",
        "kind": "Catalog",
        "releases": [{"version": "2.8.13", "platforms": [{}] * 6}],
    }
    monkeypatch.setattr(
        "ci.installer_candidate.urlopen",
        lambda *_args, **_kwargs: io.BytesIO(json.dumps(prior).encode()),
    )
    fixture = build_fixture(artifacts, tmp_path / "fixture", SOURCE_SHA, VERSION)
    assert len(fixture["releases"][0]["platforms"]) == 6
    assert fixture["releases"][1]["version"] == "2.8.13"
    require_evidence(rows, SOURCE_SHA, VERSION, artifacts)
    rows[0]["sha256"] = "f" * 64
    with pytest.raises(ValueError, match="different bytes"):
        require_evidence(rows, SOURCE_SHA, VERSION, artifacts)


def test_pr_workflow_declares_every_native_candidate_lane() -> None:
    workflow = (
        Path(__file__).resolve().parent.parent / ".github" / "workflows" / "installer-check.yml"
    ).read_text(encoding="utf-8")
    for target in REQUIRED_TARGETS:
        assert target in workflow
    assert "candidate-host:" in workflow
    assert "candidate-aggregate:" in workflow
    assert "if: always()" in workflow


def test_native_candidate_installs_by_name_and_writes_host_evidence(
    installer_target,  # noqa: F811
) -> None:
    artifact_dir = os.environ.get("CLUD_CANDIDATE_ARTIFACT")
    if not artifact_dir:
        pytest.skip("native candidate artifact is provided by its host workflow")
    target = os.environ["CLUD_CANDIDATE_TARGET"]
    source_sha = os.environ["CLUD_CANDIDATE_SHA"]
    version = os.environ["CLUD_CANDIDATE_VERSION"]
    metadata = verify_artifact(Path(artifact_dir), target, source_sha, version)
    os_name, arch, _kind, _suffix = TARGETS[target]
    actual_os = (
        "windows"
        if sys.platform == "win32"
        else "darwin"
        if sys.platform == "darwin"
        else "linux"
    )
    actual_arch = platform.machine().lower()
    actual_arch = {"amd64": "x86_64", "arm64": "aarch64"}.get(actual_arch, actual_arch)
    assert (actual_os, actual_arch) == (os_name, arch)

    binary = Path(artifact_dir) / metadata["filename"]
    env, destination = installer_target
    if actual_os == "darwin":
        env["SHELL"] = "/bin/zsh"
    env["HTTPS_PROXY"] = "http://127.0.0.1:1"
    env["HTTP_PROXY"] = "http://127.0.0.1:1"
    source_output = run_process(
        [str(binary), "--version"], env=env, capture_output=True, timeout=15, check=False
    )
    assert source_output.returncode == 0
    assert source_output.stdout.strip() == f"clud {version}".encode()
    installed = run_process(
        [str(binary), "--installer", "--install-current", "--yes"],
        env=env,
        capture_output=True,
        timeout=45,
        check=False,
    )
    assert installed.returncode == 0, installed.stderr.decode(errors="replace")
    assert hashlib.sha256(destination.read_bytes()).hexdigest() == metadata["sha256"]
    fixture_asset = (
        Path(os.environ["CLUD_INSTALLER_CI_FIXTURE_DIR"])
        / "assets"
        / metadata["filename"]
    )
    # Removing the fixture asset by rename proves same-version reuse reads the
    # running candidate rather than taking the transport branch.
    hidden_asset = fixture_asset.with_name(fixture_asset.name + ".hidden")
    fixture_asset.rename(hidden_asset)
    try:
        reused = run_process(
            [str(binary), "--installer", "--install-version", version, "--yes"],
            env=env,
            capture_output=True,
            timeout=45,
            check=False,
        )
        assert reused.returncode == 0, reused.stderr.decode(errors="replace")
    finally:
        hidden_asset.rename(fixture_asset)
    assert hashlib.sha256(destination.read_bytes()).hexdigest() == metadata["sha256"]
    selected = run_process(
        [str(binary), "--installer", "--install-version", version, "--yes"],
        env={**env, "CLUD_INSTALLER_CI_FORCE_DOWNLOAD": "1"},
        capture_output=True,
        timeout=45,
        check=False,
    )
    assert selected.returncode == 0, selected.stderr.decode(errors="replace")
    assert hashlib.sha256(destination.read_bytes()).hexdigest() == metadata["sha256"]
    original_asset = fixture_asset.read_bytes()
    try:
        fixture_asset.write_bytes(original_asset + b"tampered")
        rejected = run_process(
            [str(binary), "--installer", "--install-version", version, "--yes"],
            env={**env, "CLUD_INSTALLER_CI_FORCE_DOWNLOAD": "1"},
            capture_output=True,
            timeout=45,
            check=False,
        )
        assert rejected.returncode != 0
        assert hashlib.sha256(destination.read_bytes()).hexdigest() == metadata["sha256"]
    finally:
        fixture_asset.write_bytes(original_asset)

    if actual_os == "windows":
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
        shell = "/bin/zsh" if actual_os == "darwin" else "/bin/bash"
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
    assert len(lines) == 2, lines
    assert Path(lines[0]) == destination
    assert lines[1] == f"clud {version}"
    evidence = {
        "target": target,
        "source_sha": source_sha,
        "version": version,
        "sha256": hashlib.sha256(destination.read_bytes()).hexdigest(),
        "host_os": actual_os,
        "host_arch": actual_arch,
        "resolved_path": lines[0],
        "version_output": lines[1],
        "result": "success",
    }
    Path(os.environ["CLUD_CANDIDATE_EVIDENCE"]).write_text(
        json.dumps(evidence, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
