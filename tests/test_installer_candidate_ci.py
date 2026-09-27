"""A PR candidate must keep its source and six native-host results together."""

from __future__ import annotations

import hashlib
import json

import pytest

from ci.installer_candidate import REQUIRED_TARGETS, require_evidence, verify_artifact
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
    with pytest.raises(ValueError, match="digest|size|format"):
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
    with pytest.raises(ValueError, match="success"):
        require_evidence(
            [*rows[:-1], {**rows[-1], "result": "skipped"}], SOURCE_SHA, VERSION
        )
    with pytest.raises(ValueError, match="source"):
        require_evidence(
            [*rows[:-1], {**rows[-1], "source_sha": "b" * 40}], SOURCE_SHA, VERSION
        )


def test_pr_workflow_declares_every_native_candidate_lane() -> None:
    from pathlib import Path

    workflow = (
        Path(__file__).resolve().parent.parent / ".github" / "workflows" / "installer-check.yml"
    ).read_text(encoding="utf-8")
    for target in REQUIRED_TARGETS:
        assert target in workflow
    assert "candidate-host:" in workflow
    assert "candidate-aggregate:" in workflow
    assert "if: always()" in workflow
