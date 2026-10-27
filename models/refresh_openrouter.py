"""Refresh the bundled stable Codex model slugs from OpenRouter's public API."""

from __future__ import annotations

import json
import re
from datetime import datetime, timezone
from pathlib import Path

from models.publish import OPENROUTER_MODELS, _model_ids, fetch_json

MODEL_ID = re.compile(r"^gpt-([1-9][0-9]*)(?:\.([0-9]+))?-(sol|luna|terra)$")
DESTINATION = Path(__file__).resolve().parent.parent / "crates/clud-bin/assets/codex-models.json"


def latest_models(ids: set[str]) -> dict[str, str]:
    """Pick one stable model per family, excluding preview and pro variants."""
    found: dict[str, tuple[tuple[int, int], str]] = {}
    for model_id in ids:
        match = MODEL_ID.fullmatch(model_id)
        if match is None:
            continue
        major, minor, family = match.groups()
        entry = ((int(major), int(minor or 0)), model_id)
        if family not in found or entry > found[family]:
            found[family] = entry
    if set(found) != {"sol", "luna", "terra"}:
        raise ValueError("OpenRouter is missing a stable Sol, Luna, or Terra model")
    return {family: found[family][1] for family in ("sol", "terra", "luna")}


def main() -> None:
    ids = _model_ids(fetch_json(OPENROUTER_MODELS), openrouter=True)
    document = {
        "source": OPENROUTER_MODELS,
        "checked_at": datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
        "families": latest_models(ids),
    }
    DESTINATION.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
