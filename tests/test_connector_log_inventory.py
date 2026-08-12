from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path

from bench.connector_logs.inventory import (
    attribute_bridge,
    inventory_bridge,
    inventory_transcript,
    report_text,
    safe_error_labels,
    safe_http_status,
)


def write_jsonl(path: Path, records: list[dict[str, object]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "".join(f"{json.dumps(record)}\n" for record in records),
        encoding="utf-8",
    )


def assistant(cwd: Path, timestamp: str, model: str) -> dict[str, object]:
    return {
        "type": "assistant",
        "cwd": str(cwd),
        "timestamp": timestamp,
        "message": {"model": model, "stop_reason": "end_turn"},
    }


def test_inventory_attributes_bridge_without_emitting_raw_error_text(tmp_path: Path) -> None:
    project = tmp_path / "repo"
    project.mkdir()
    transcript_path = tmp_path / "session.jsonl"
    write_jsonl(
        transcript_path,
        [
            assistant(project, "2026-08-12T12:00:00Z", "gpt-5.6-terra"),
            {
                "type": "user",
                "cwd": str(project),
                "timestamp": "2026-08-12T12:00:01Z",
                "isApiErrorMessage": True,
                "apiErrorStatus": 400,
                "error": "context_length_exceeded secret prompt fragment",
            },
        ],
    )
    transcript = inventory_transcript(transcript_path, project)
    assert transcript.usable
    assert transcript.providers == ["codex"]
    assert transcript.api_error_statuses == {"400": 1}
    assert transcript.error_labels == {"context_length_exceeded": 1}

    epoch = int(datetime(2026, 8, 12, 12, 0, 5, tzinfo=UTC).timestamp())
    bridge_path = tmp_path / f"123__{epoch}" / "bridge.jsonl"
    write_jsonl(
        bridge_path,
        [
            {
                "event": "in_band_upstream_failure",
                "upstream_status": 400,
                "code": "context_length_exceeded",
            },
            {
                "event": "in_band_upstream_failure",
                "upstream_status": 400,
                "code": "private provider prose",
            },
        ],
    )
    bridge = inventory_bridge(bridge_path)
    attribute_bridge(bridge, [transcript], window_seconds=120)
    assert bridge.usable
    assert bridge.attribution == "exact"
    assert bridge.providers == ["codex"]
    assert bridge.codes == {"context_length_exceeded": 1}

    report = report_text(project, [transcript], [bridge], show_unusable=False)
    assert "context_length_exceeded" in report
    assert "secret prompt fragment" not in report


def test_deepseek_transcript_is_usable_without_a_bridge(tmp_path: Path) -> None:
    project = tmp_path / "repo"
    project.mkdir()
    transcript_path = tmp_path / "deepseek.jsonl"
    write_jsonl(
        transcript_path,
        [assistant(project, "2026-08-12T12:00:00Z", "deepseek-v4-pro")],
    )
    transcript = inventory_transcript(transcript_path, project)
    assert transcript.usable
    assert transcript.providers == ["deepseek"]
    assert "usable connector transcripts" in report_text(
        project, [transcript], [], show_unusable=False
    )


def test_unmatched_bridge_stays_unattributed(tmp_path: Path) -> None:
    bridge_path = tmp_path / "123__1786536000" / "bridge.jsonl"
    write_jsonl(bridge_path, [{"event": "pipeline_failure", "kind": "timeout"}])
    bridge = inventory_bridge(bridge_path)
    attribute_bridge(bridge, [], window_seconds=120)
    assert not bridge.usable
    assert bridge.attribution == "unattributed"


def test_correlated_bridge_with_rust_test_process_is_rejected(tmp_path: Path) -> None:
    project = tmp_path / "repo"
    project.mkdir()
    transcript_path = tmp_path / "session.jsonl"
    write_jsonl(
        transcript_path,
        [assistant(project, "2026-08-12T12:00:00Z", "gpt-5.6-terra")],
    )
    transcript = inventory_transcript(transcript_path, project)
    epoch = int(datetime(2026, 8, 12, 12, 0, 5, tzinfo=UTC).timestamp())
    bridge_path = tmp_path / f"123__{epoch}" / "bridge.jsonl"
    write_jsonl(bridge_path, [{"event": "pipeline_failure", "kind": "status"}])
    write_jsonl(
        bridge_path.with_name("reap.jsonl"),
        [{"image_name": "cargo.exe", "action": "reaped"}],
    )

    bridge = inventory_bridge(bridge_path)
    attribute_bridge(bridge, [transcript], window_seconds=120)
    assert not bridge.usable
    assert bridge.attribution == "test_contaminated"
    assert bridge.contamination_reasons == ["rust_build_or_test_process"]


def test_error_label_extraction_is_allowlisted() -> None:
    assert safe_error_labels("rate_limit_exceeded private provider prose") == [
        "rate_limit_exceeded"
    ]
    assert safe_error_labels("private provider prose") == []


def test_http_status_extraction_rejects_arbitrary_text() -> None:
    assert safe_http_status(400) == "400"
    assert safe_http_status("503") == "503"
    assert safe_http_status("400 private provider prose") is None
    assert safe_http_status(True) is None
