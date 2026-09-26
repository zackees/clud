"""Build the small, strict Codex model manifest published on Pages."""

from __future__ import annotations

import re
from collections.abc import Iterable
from datetime import datetime, timedelta, timezone
from typing import Any

_MODEL_ID = re.compile(r"^gpt-([1-9][0-9]*)(?:\.([0-9]+))?-(sol|luna)$")
SCHEMA_VERSION = 1
SOURCE = "openrouter+models.dev-merge"
LEGACY_SOURCE = "openai-models+openrouter-cross-check"
BUILT_IN_FAMILIES = {"sol": "gpt-6-sol", "luna": "gpt-6-luna"}


def _timestamp(value: str) -> datetime:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include a UTC offset")
    return parsed.astimezone(timezone.utc)


def build_manifest(
    model_ids: Iterable[str],
    *,
    checked_at: str,
    trigger: str = "manual",
    source: str = SOURCE,
) -> dict[str, Any]:
    """Select the highest stable Sol and Luna IDs from a verified catalog."""
    if trigger not in {"manual", "nightly"}:
        raise ValueError("trigger must be manual or nightly")
    _timestamp(checked_at)
    candidates: dict[str, tuple[tuple[int, int], str]] = {}
    for model_id in model_ids:
        match = _MODEL_ID.fullmatch(model_id)
        if match is None:
            continue
        major, minor, family = match.groups()
        version = (int(major), int(minor or 0))
        if family not in candidates or (version, model_id) > candidates[family]:
            candidates[family] = (version, model_id)
    missing = {"sol", "luna"} - candidates.keys()
    if missing:
        raise ValueError(f"missing stable model family: {', '.join(sorted(missing))}")
    families = {family: candidates[family][1] for family in ("sol", "luna")}
    return {
        "schema_version": SCHEMA_VERSION,
        "source": source,
        "checked_at": checked_at,
        "trigger": trigger,
        "families": families,
        "defaults": {"codex": {"model": families["sol"], "effort": "low"}},
    }


def validate_manifest(document: dict[str, Any]) -> None:
    """Reject malformed or internally inconsistent published documents."""
    if (
        not {"schema_version", "source", "checked_at", "trigger", "families", "defaults"}.issubset(
            document
        )
        or document["schema_version"] != SCHEMA_VERSION
    ):
        raise ValueError("unsupported model manifest schema")
    if document["source"] not in {SOURCE, LEGACY_SOURCE}:
        raise ValueError("untrusted model manifest source")
    if document["trigger"] not in {"manual", "nightly"}:
        raise ValueError("invalid model manifest trigger")
    _timestamp(document["checked_at"])
    families = document["families"]
    if not isinstance(families, dict) or not {"sol", "luna"}.issubset(families):
        raise ValueError("invalid model families")
    for family in ("sol", "luna"):
        model_id = families[family]
        if not isinstance(model_id, str) or not _MODEL_ID.fullmatch(model_id):
            raise ValueError(f"invalid {family} model ID")
        if not model_id.endswith(f"-{family}"):
            raise ValueError(f"mismatched {family} model ID")
    defaults = document["defaults"]
    if not isinstance(defaults, dict) or not isinstance(defaults.get("codex"), dict):
        raise ValueError("invalid Codex default")
    codex = defaults["codex"]
    if codex.get("model") != families["sol"] or codex.get("effort") != "low":
        raise ValueError("invalid Codex default")


def merge_manifest(
    previous: dict[str, Any] | None,
    observed_ids: Iterable[str],
    *,
    checked_at: str,
    trigger: str,
) -> dict[str, Any]:
    """Only advance stable family IDs; a missing catalog row never erases one."""
    baseline = previous["families"] if previous else {}
    return build_manifest(
        [*BUILT_IN_FAMILIES.values(), *baseline.values(), *observed_ids],
        checked_at=checked_at,
        trigger=trigger,
        source=SOURCE,
    )


def should_publish(previous: dict[str, Any] | None, *, now: datetime, trigger: str) -> bool:
    """Skip a nightly check when a manual publication succeeded within 24 hours."""
    if trigger != "nightly" or not previous or previous.get("trigger") != "manual":
        return True
    try:
        checked_at = _timestamp(previous["checked_at"])
    except (KeyError, TypeError, ValueError):
        return True
    if now.tzinfo is None:
        raise ValueError("now must include a UTC offset")
    age = now.astimezone(timezone.utc) - checked_at
    return not (timedelta(0) <= age < timedelta(hours=24))
