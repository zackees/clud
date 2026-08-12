"""Identify Claude and clud logs usable for connector error analysis.

The inventory deliberately reads only structured metadata. It never emits
prompts, responses, tool payloads, or raw provider error strings.
"""

from __future__ import annotations

import argparse
import json
import os
import re
from collections import Counter
from collections.abc import Iterable
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

CONNECTOR_MODEL_PREFIXES = {
    "codex": ("gpt-", "codex"),
    "deepseek": ("deepseek-",),
}

# These labels are control-flow metadata, not provider-authored prose. Keep the
# list explicit so a future error message cannot accidentally reach stdout.
SAFE_ERROR_LABELS = (
    "api_error",
    "authentication_error",
    "billing_error",
    "cancelled",
    "context_length_exceeded",
    "insufficient_quota",
    "invalid_request_error",
    "model_context_window_exceeded",
    "overloaded_error",
    "quota_exceeded",
    "rate_limit_exceeded",
    "timeout",
    "transport",
    "usage_limit_reached",
)

MIN_UTC = datetime.min.replace(tzinfo=UTC)


@dataclass
class TranscriptInventory:
    path: str
    session_id: str
    project_matches: bool
    start: datetime | None = None
    end: datetime | None = None
    providers: list[str] = field(default_factory=list)
    models: list[str] = field(default_factory=list)
    assistant_records: int = 0
    api_error_statuses: dict[str, int] = field(default_factory=dict)
    error_labels: dict[str, int] = field(default_factory=dict)
    stop_reasons: dict[str, int] = field(default_factory=dict)
    malformed_lines: int = 0

    @property
    def usable(self) -> bool:
        return self.project_matches and bool(self.providers) and self.start is not None


@dataclass
class BridgeInventory:
    path: str
    process_dir: str
    start: datetime | None
    records: int = 0
    malformed_lines: int = 0
    events: dict[str, int] = field(default_factory=dict)
    reasons: dict[str, int] = field(default_factory=dict)
    kinds: dict[str, int] = field(default_factory=dict)
    downstream_statuses: dict[str, int] = field(default_factory=dict)
    upstream_statuses: dict[str, int] = field(default_factory=dict)
    classes: dict[str, int] = field(default_factory=dict)
    codes: dict[str, int] = field(default_factory=dict)
    contamination_reasons: list[str] = field(default_factory=list)
    attribution: str = "unattributed"
    providers: list[str] = field(default_factory=list)
    transcript_ids: list[str] = field(default_factory=list)
    start_delta_seconds: float | None = None

    @property
    def usable(self) -> bool:
        return self.attribution in {"exact", "provider_only"}


def parse_timestamp(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=UTC)
    return parsed.astimezone(UTC)


def connector_provider(model: Any) -> str | None:
    if not isinstance(model, str) or model == "<synthetic>":
        return None
    lowered = model.lower()
    for provider, prefixes in CONNECTOR_MODEL_PREFIXES.items():
        if lowered.startswith(prefixes):
            return provider
    return None


def safe_error_labels(value: Any) -> list[str]:
    """Return only allowlisted labels found in a structured error value."""
    strings: list[str] = []
    if isinstance(value, str):
        strings.append(value.lower())
    elif isinstance(value, dict):
        for key in ("code", "type", "kind", "category", "reason"):
            candidate = value.get(key)
            if isinstance(candidate, str):
                strings.append(candidate.lower())
    labels = {
        label
        for text in strings
        for label in SAFE_ERROR_LABELS
        if re.search(rf"(?<![a-z0-9_]){re.escape(label)}(?![a-z0-9_])", text)
    }
    return sorted(labels)


def safe_http_status(value: Any) -> str | None:
    """Return a normalized HTTP status without forwarding arbitrary text."""
    if isinstance(value, bool) or not isinstance(value, (int, str)):
        return None
    text = str(value)
    if not re.fullmatch(r"[1-5]\d{2}", text):
        return None
    return text


def _counter_dict(counter: Counter[str]) -> dict[str, int]:
    return dict(sorted(counter.items()))


def inventory_transcript(path: Path, project: Path) -> TranscriptInventory:
    models: set[str] = set()
    providers: set[str] = set()
    statuses: Counter[str] = Counter()
    labels: Counter[str] = Counter()
    stop_reasons: Counter[str] = Counter()
    start: datetime | None = None
    end: datetime | None = None
    project_matches = False
    assistant_records = 0
    malformed = 0
    expected_project = os.path.normcase(str(project.resolve()))

    with path.open("r", encoding="utf-8", errors="replace") as stream:
        for line in stream:
            try:
                record = json.loads(line)
            except (json.JSONDecodeError, UnicodeError):
                malformed += 1
                continue
            if not isinstance(record, dict):
                continue
            timestamp = parse_timestamp(record.get("timestamp"))
            if timestamp is not None:
                start = timestamp if start is None else min(start, timestamp)
                end = timestamp if end is None else max(end, timestamp)

            cwd = record.get("cwd")
            if isinstance(cwd, str):
                try:
                    normalized_cwd = os.path.normcase(str(Path(cwd).resolve()))
                except OSError:
                    normalized_cwd = os.path.normcase(os.path.abspath(cwd))
                project_matches |= normalized_cwd == expected_project

            message = record.get("message")
            if isinstance(message, dict) and record.get("type") == "assistant":
                assistant_records += 1
                model = message.get("model")
                provider = connector_provider(model)
                if provider is not None:
                    providers.add(provider)
                    models.add(str(model))
                stop_reason = message.get("stop_reason")
                if isinstance(stop_reason, str) and stop_reason:
                    stop_reasons[stop_reason] += 1

            if record.get("isApiErrorMessage") is True:
                status = safe_http_status(record.get("apiErrorStatus"))
                if status is not None:
                    statuses[status] += 1
                labels.update(safe_error_labels(record.get("error")))
            elif record.get("type") == "error":
                labels.update(safe_error_labels(record.get("error")))

    return TranscriptInventory(
        path=str(path),
        session_id=path.stem,
        project_matches=project_matches,
        start=start,
        end=end,
        providers=sorted(providers),
        models=sorted(models),
        assistant_records=assistant_records,
        api_error_statuses=_counter_dict(statuses),
        error_labels=_counter_dict(labels),
        stop_reasons=_counter_dict(stop_reasons),
        malformed_lines=malformed,
    )


def bridge_start(process_dir: str) -> datetime | None:
    match = re.fullmatch(r"\d+__(\d+)", process_dir)
    if match is None:
        return None
    try:
        return datetime.fromtimestamp(int(match.group(1)), tz=UTC)
    except (OSError, OverflowError, ValueError):
        return None


def inventory_bridge(path: Path) -> BridgeInventory:
    events: Counter[str] = Counter()
    reasons: Counter[str] = Counter()
    kinds: Counter[str] = Counter()
    downstream: Counter[str] = Counter()
    upstream: Counter[str] = Counter()
    classes: Counter[str] = Counter()
    codes: Counter[str] = Counter()
    records = 0
    malformed = 0

    with path.open("r", encoding="utf-8", errors="replace") as stream:
        for line in stream:
            try:
                record = json.loads(line)
            except (json.JSONDecodeError, UnicodeError):
                malformed += 1
                continue
            if not isinstance(record, dict):
                continue
            records += 1
            for key, counter in (
                ("event", events),
                ("reason", reasons),
                ("kind", kinds),
                ("class", classes),
            ):
                value = record.get(key)
                if isinstance(value, (int, str)) and str(value):
                    counter[str(value)] += 1
            for key, counter in (
                ("downstream_status", downstream),
                ("upstream_status", upstream),
            ):
                status = safe_http_status(record.get(key))
                if status is not None:
                    counter[status] += 1
            codes.update(safe_error_labels(record.get("code")))

    contamination_reasons: list[str] = []
    reap_path = path.with_name("reap.jsonl")
    if reap_path.is_file() and reap_has_rust_build_or_test_process(reap_path):
        contamination_reasons.append("rust_build_or_test_process")
    if (
        reasons["bearer_mismatch"]
        and reasons["token_counting_unsupported"]
        and reasons["admission_cap"]
    ):
        contamination_reasons.append("bridge_fixture_matrix")
    if downstream["400"] >= 40 and kinds["status"] >= 40:
        contamination_reasons.append("repeated_400_fixture_pattern")

    process_dir = path.parent.name
    return BridgeInventory(
        path=str(path),
        process_dir=process_dir,
        start=bridge_start(process_dir),
        records=records,
        malformed_lines=malformed,
        events=_counter_dict(events),
        reasons=_counter_dict(reasons),
        kinds=_counter_dict(kinds),
        downstream_statuses=_counter_dict(downstream),
        upstream_statuses=_counter_dict(upstream),
        classes=_counter_dict(classes),
        codes=_counter_dict(codes),
        contamination_reasons=contamination_reasons,
    )


def reap_has_rust_build_or_test_process(path: Path) -> bool:
    """Conservatively identify a bridge log that may include Rust test output."""
    with path.open("r", encoding="utf-8", errors="replace") as stream:
        for line in stream:
            try:
                record = json.loads(line)
            except (json.JSONDecodeError, UnicodeError):
                continue
            if not isinstance(record, dict):
                continue
            image_name = record.get("image_name")
            if not isinstance(image_name, str):
                continue
            if image_name.lower() in {"cargo", "cargo.exe", "rustc", "rustc.exe"}:
                return True
    return False


def attribute_bridge(
    bridge: BridgeInventory,
    transcripts: Iterable[TranscriptInventory],
    window_seconds: float,
) -> None:
    if bridge.start is None:
        bridge.attribution = "invalid_start"
        return
    candidates: list[tuple[float, TranscriptInventory]] = []
    for transcript in transcripts:
        if not transcript.usable or transcript.start is None:
            continue
        delta = abs((bridge.start - transcript.start).total_seconds())
        if delta <= window_seconds:
            candidates.append((delta, transcript))
    if not candidates:
        bridge.attribution = "unattributed"
        return

    best_delta = min(delta for delta, _ in candidates)
    # Several transcript files can be created by the same harness launch. Keep
    # candidates in the same one-second start cluster, not every loose match in
    # the caller's correlation window.
    nearest = [item for delta, item in candidates if delta <= best_delta + 1.0]
    bridge.providers = sorted({provider for item in nearest for provider in item.providers})
    bridge.transcript_ids = sorted(item.session_id for item in nearest)
    bridge.start_delta_seconds = round(best_delta, 3)
    if bridge.contamination_reasons:
        bridge.attribution = "test_contaminated"
    else:
        bridge.attribution = "exact" if len(nearest) == 1 else "provider_only"


def encoded_project_dir(project: Path) -> str:
    return str(project.resolve()).replace(":", "-").replace("\\", "-").replace("/", "-")


def discover_transcripts(claude_root: Path, project: Path) -> list[Path]:
    direct = claude_root / encoded_project_dir(project)
    if direct.is_dir():
        return sorted(direct.glob("*.jsonl"))
    # Fallback for a client that changes its directory encoding. The content
    # pass still requires an exact structured `cwd` match before use.
    return sorted(claude_root.glob("*/*.jsonl"))


def iso(value: datetime | None) -> str | None:
    return value.isoformat().replace("+00:00", "Z") if value is not None else None


def json_ready(value: TranscriptInventory | BridgeInventory) -> dict[str, Any]:
    result = asdict(value)
    result["start"] = iso(value.start)
    if isinstance(value, TranscriptInventory):
        result["end"] = iso(value.end)
        result["usable"] = value.usable
    else:
        result["usable"] = value.usable
    return result


def report_text(
    project: Path,
    transcripts: list[TranscriptInventory],
    bridges: list[BridgeInventory],
    show_unusable: bool,
) -> str:
    lines = [f"project: {project.resolve()}", "", "usable connector transcripts:"]
    usable_transcripts = [item for item in transcripts if item.usable]
    if not usable_transcripts:
        lines.append("  none")
    for item in sorted(usable_transcripts, key=lambda row: row.start or MIN_UTC, reverse=True):
        lines.append(
            "  "
            f"{item.session_id[:8]} providers={','.join(item.providers)} "
            f"models={','.join(item.models)} start={iso(item.start)} "
            f"api_statuses={item.api_error_statuses or '{}'} "
            f"error_labels={item.error_labels or '{}'}"
        )

    usable_bridges = [item for item in bridges if item.usable]
    lines.extend(["", "usable attributed bridge logs:"])
    if not usable_bridges:
        lines.append("  none")
    for item in sorted(usable_bridges, key=lambda row: row.start or MIN_UTC, reverse=True):
        lines.append(
            "  "
            f"{item.process_dir} attribution={item.attribution} "
            f"providers={','.join(item.providers)} delta={item.start_delta_seconds}s "
            f"records={item.records} downstream={item.downstream_statuses or '{}'} "
            f"upstream={item.upstream_statuses or '{}'} codes={item.codes or '{}'}"
        )

    unusable_counts = Counter(item.attribution for item in bridges if not item.usable)
    lines.extend(["", f"unusable bridge logs by reason: {_counter_dict(unusable_counts)}"])
    if show_unusable:
        for item in sorted(
            (row for row in bridges if not row.usable),
            key=lambda row: row.start or MIN_UTC,
            reverse=True,
        ):
            lines.append(
                f"  {item.process_dir} attribution={item.attribution} records={item.records} "
                f"contamination={item.contamination_reasons or '[]'}"
            )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument(
        "--claude-root",
        type=Path,
        default=Path.home() / ".claude" / "projects",
    )
    parser.add_argument(
        "--clud-state",
        type=Path,
        default=Path.home() / ".clud" / "state" / "sessions",
    )
    parser.add_argument("--window-seconds", type=float, default=120.0)
    parser.add_argument(
        "--since-days",
        type=float,
        default=14.0,
        help="ignore logs older than this many days; 0 means all logs",
    )
    parser.add_argument("--json", action="store_true", dest="as_json")
    parser.add_argument("--show-unusable", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    project = args.project.resolve()
    cutoff = (
        datetime.now(tz=UTC) - timedelta(days=args.since_days)
        if args.since_days > 0
        else None
    )

    transcript_paths = discover_transcripts(args.claude_root, project)
    transcripts = [inventory_transcript(path, project) for path in transcript_paths]
    transcripts = [
        item
        for item in transcripts
        if item.project_matches and (cutoff is None or item.end is None or item.end >= cutoff)
    ]

    bridge_paths = (
        sorted(args.clud_state.glob("*/bridge.jsonl")) if args.clud_state.is_dir() else []
    )
    bridges = [inventory_bridge(path) for path in bridge_paths]
    bridges = [
        item for item in bridges if cutoff is None or item.start is None or item.start >= cutoff
    ]
    for bridge in bridges:
        attribute_bridge(bridge, transcripts, args.window_seconds)

    if args.as_json:
        print(
            json.dumps(
                {
                    "project": str(project),
                    "transcripts": [json_ready(item) for item in transcripts if item.usable],
                    "bridges": [json_ready(item) for item in bridges],
                },
                indent=2,
                sort_keys=True,
            )
        )
    else:
        print(report_text(project, transcripts, bridges, args.show_unusable))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
