"""Build the deterministic OpenRouter pricing catalog used by clud (#1256)."""

from __future__ import annotations

import json
import math
import sys
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

DATASHEET_URL = "https://openrouter.ai/api/v1/models"
ASSET_PATH = (
    Path(__file__).resolve().parent.parent / "crates/clud-bin/assets/openrouter-catalog.json"
)
SCHEMA_VERSION = 1
INPUT_WEIGHT, OUTPUT_WEIGHT, CACHED_INPUT_WEIGHT = 0.70, 0.20, 0.10
TOP_LIMIT = 20


class RefreshError(RuntimeError):
    """The upstream response is unsafe to publish."""


def fetch_rows(timeout: float = 30.0) -> list[dict[str, Any]]:
    request = urllib.request.Request(
        DATASHEET_URL, headers={"User-Agent": "clud-openrouter-catalog/1"}
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = json.load(response)
    if (
        not isinstance(payload, dict)
        or not isinstance(payload.get("data"), list)
        or not payload["data"]
    ):
        raise RefreshError("datasheet root must contain a non-empty `data` list")
    return payload["data"]


def _number(value: Any, model_id: str, field: str) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        raise RefreshError(f"{model_id}: invalid pricing {field}: {value!r}") from None
    if not math.isfinite(number):
        raise RefreshError(f"{model_id}: invalid pricing {field}: {value!r}")
    # OpenRouter's automatic router uses -1 as an "unknown cost" sentinel.
    # Preserve that fact as null; never rank it as a free or negative price.
    return None if number < 0 else number


def normalize_row(row: Any, index: int) -> dict[str, Any]:
    if not isinstance(row, dict):
        raise RefreshError(f"row {index} is not an object")
    model_id, name = row.get("id"), row.get("name")
    if (
        not isinstance(model_id, str)
        or not model_id
        or len(model_id) > 128
        or not all(char.isascii() and (char.isalnum() or char in "._-/:~") for char in model_id)
    ):
        raise RefreshError(f"row {index} has an invalid model id: {model_id!r}")
    if not isinstance(name, str) or not name.strip():
        raise RefreshError(f"{model_id}: missing name")
    context = row.get("context_length")
    if (
        not isinstance(context, int)
        or isinstance(context, bool)
        or not 1_000 <= context <= 10_000_000
    ):
        raise RefreshError(f"{model_id}: invalid context_length {context!r}")
    pricing = row.get("pricing")
    if not isinstance(pricing, dict):
        raise RefreshError(f"{model_id}: missing pricing object")
    input_price = _number(pricing.get("prompt"), model_id, "prompt")
    output_price = _number(pricing.get("completion"), model_id, "completion")
    if "input_cache_read" in pricing:
        cached_price = _number(pricing["input_cache_read"], model_id, "input_cache_read")
    else:
        cached_price = input_price
    if cached_price is None:
        cached_price = input_price
    architecture = row.get("architecture")
    if not isinstance(architecture, dict):
        raise RefreshError(f"{model_id}: missing architecture")
    modalities, output_modalities = (
        architecture.get("input_modalities"),
        architecture.get("output_modalities"),
    )
    parameters = row.get("supported_parameters")
    if not all(
        isinstance(value, list) and all(isinstance(item, str) for item in value)
        for value in (modalities, output_modalities, parameters)
    ):
        raise RefreshError(f"{model_id}: malformed modalities or supported_parameters")
    tools = "tools" in parameters and "tool_choice" in parameters
    is_text_model = "text" in modalities and "text" in output_modalities
    eligible = (
        is_text_model
        and tools
        and context >= 16_000
        and input_price is not None
        and output_price is not None
    )
    reasons = []
    if not is_text_model:
        reasons.append("requires text input and output")
    if not tools:
        reasons.append("requires tools and tool_choice support")
    if context < 16_000:
        reasons.append("requires at least 16000 context tokens")
    if input_price is None or output_price is None:
        reasons.append("requires known input and output prices")
    return {
        "id": model_id,
        "name": name.strip(),
        "provider": model_id.split("/", 1)[0].lstrip("~"),
        "context_length": context,
        "input_price_per_token": input_price,
        "output_price_per_token": output_price,
        "cached_input_price_per_token": cached_price,
        "supports_tools": tools,
        "supports_text_input": "text" in modalities,
        "supports_text_output": "text" in output_modalities,
        "supports_reasoning": "reasoning" in parameters or "include_reasoning" in parameters,
        "supports_vision": "image" in modalities,
        "eligible_for_coding": eligible,
        "ineligibility_reasons": reasons,
    }


def weighted_cost(model: dict[str, Any]) -> float:
    return 1_000_000 * (
        model["input_price_per_token"] * INPUT_WEIGHT
        + model["output_price_per_token"] * OUTPUT_WEIGHT
        + model["cached_input_price_per_token"] * CACHED_INPUT_WEIGHT
    )


def normalize(rows: list[Any], generated_at: str | None = None) -> dict[str, Any]:
    models = [normalize_row(row, index) for index, row in enumerate(rows)]
    if not models:
        raise RefreshError("datasheet produced no models")
    models.sort(key=lambda model: model["id"])
    if len({model["id"] for model in models}) != len(models):
        raise RefreshError("datasheet contains duplicate model IDs")
    eligible = sorted(
        (model for model in models if model["eligible_for_coding"]),
        key=lambda model: (weighted_cost(model), model["id"]),
    )
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at": generated_at
        or datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
        "source": DATASHEET_URL,
        "pricing_estimate": {
            "label": (
                "lowest-priced eligible models by weighted token-mix estimate; "
                "not a quality ranking"
            ),
            "input_weight": INPUT_WEIGHT,
            "output_weight": OUTPUT_WEIGHT,
            "cached_input_weight": CACHED_INPUT_WEIGHT,
            "usd_per_million_tokens": "input*0.70 + output*0.20 + cached_input*0.10",
        },
        "eligibility": {
            "requires": [
                "text input",
                "text output",
                "tools",
                "tool_choice",
                "context >= 16000",
                "known input and output prices",
            ],
            "note": (
                "OpenRouter does not publish a coding-quality score; eligible is "
                "a capability and pricing filter."
            ),
        },
        "models": models,
        "cheapest_programming": [
            {"id": model["id"], "weighted_usd_per_million_tokens": round(weighted_cost(model), 12)}
            for model in eligible[:TOP_LIMIT]
        ],
    }


def rewrite(original: str, document: dict[str, Any]) -> str:
    try:
        previous = json.loads(original)
    except json.JSONDecodeError:
        previous = None
    if isinstance(previous, dict):
        current_data = {key: value for key, value in document.items() if key != "generated_at"}
        previous_data = {key: value for key, value in previous.items() if key != "generated_at"}
        if current_data == previous_data and isinstance(previous.get("generated_at"), str):
            document = dict(document, generated_at=previous["generated_at"])
    return json.dumps(document, ensure_ascii=False, indent=2, allow_nan=False) + "\n"


def main(argv: list[str] | None = None) -> int:
    del argv
    try:
        document = normalize(fetch_rows())
        try:
            original = ASSET_PATH.read_text(encoding="utf-8")
        except FileNotFoundError:
            original = ""
        updated = rewrite(original, document)
    except RefreshError as error:
        print(f"refresh_openrouter_catalog: {error}", file=sys.stderr)
        return 1
    except (OSError, ValueError) as error:
        print(f"refresh_openrouter_catalog: fetch or parse failed: {error}", file=sys.stderr)
        return 1
    if updated == original:
        print("OpenRouter catalog already fresh")
        return 0
    ASSET_PATH.write_text(updated, encoding="utf-8")
    print(f"OpenRouter catalog refreshed ({len(document['models'])} models)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
