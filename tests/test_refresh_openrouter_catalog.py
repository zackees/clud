"""Offline contract tests for the OpenRouter catalog producer (#1256)."""

from __future__ import annotations

import json

import pytest

from ci import refresh_openrouter_catalog as producer


def _row(
    model_id: str,
    input_price: str,
    output_price: str,
    cached_price: str | None,
    *,
    tools: bool = True,
    context: int = 32_000,
) -> dict:
    pricing = {"prompt": input_price, "completion": output_price}
    if cached_price is not None:
        pricing["input_cache_read"] = cached_price
    return {
        "id": model_id,
        "name": model_id,
        "context_length": context,
        "pricing": pricing,
        "architecture": {
            "input_modalities": ["text", "image"],
            "output_modalities": ["text"],
        },
        "supported_parameters": ["tools", "tool_choice", "reasoning"] if tools else [],
    }


def test_eligibility_and_weighted_ranking_differ_from_input_only() -> None:
    rows = [
        _row("provider/input-cheap", "0.0000001", "0.00001", None),
        _row("provider/weighted-cheap", "0.0000002", "0.00000001", "0.00000001"),
        _row("provider/no-tools", "0.000000001", "0.000000001", None, tools=False),
        _row("provider/short-context", "0.000000001", "0.000000001", None, context=8_000),
    ]
    result = producer.normalize(rows, generated_at="2026-09-23T00:00:00Z")
    assert [row["id"] for row in result["cheapest_programming"]] == [
        "provider/weighted-cheap",
        "provider/input-cheap",
    ]
    by_id = {row["id"]: row for row in result["models"]}
    assert by_id["provider/no-tools"]["eligible_for_coding"] is False
    assert by_id["provider/short-context"]["eligible_for_coding"] is False
    assert by_id["provider/input-cheap"]["cached_input_price_per_token"] == 0.0000001
    assert result["pricing_estimate"]["input_weight"] == 0.7
    assert "not a quality ranking" in result["pricing_estimate"]["label"]
    input_only = min(
        (row for row in result["models"] if row["eligible_for_coding"]),
        key=lambda row: row["input_price_per_token"],
    )
    assert input_only["id"] == "provider/input-cheap"


def test_free_models_can_be_eligible() -> None:
    result = producer.normalize([_row("provider/free-coder", "0", "0", None)])
    assert result["models"][0]["eligible_for_coding"] is True
    assert result["cheapest_programming"][0]["weighted_usd_per_million_tokens"] == 0


def test_negative_upstream_price_sentinel_is_preserved_as_unrankable() -> None:
    result = producer.normalize([_row("openrouter/auto-beta", "-1", "-1", None)])
    model = result["models"][0]
    assert model["input_price_per_token"] is None
    assert model["output_price_per_token"] is None
    assert model["eligible_for_coding"] is False
    assert result["cheapest_programming"] == []


@pytest.mark.parametrize(
    "rows",
    [
        [],
        ["not an object"],
        [_row("bad id", "1", "1", None)],
        [_row("provider/missing-price", "x", "1", None)],
        [_row("provider/bad-context", "1", "1", None, context=999)],
        [_row("provider/duplicate", "1", "1", None)] * 2,
    ],
)
def test_malformed_rows_fail_the_whole_catalog(rows: list[object]) -> None:
    with pytest.raises(producer.RefreshError):
        producer.normalize(rows)


def test_output_is_sorted_strict_json_and_identical_for_identical_input() -> None:
    normalized = producer.normalize(
        [_row("z/provider", "0.1", "0.2", None), _row("a/provider", "0.1", "0.2", None)],
        generated_at="2026-09-23T00:00:00Z",
    )
    first = producer.rewrite("", normalized)
    parsed = json.loads(first)
    assert [model["id"] for model in parsed["models"]] == ["a/provider", "z/provider"]
    assert first == producer.rewrite(first, normalized)
    assert first.endswith("\n")


def test_failed_fetch_is_visible_and_leaves_existing_file_untouched(
    monkeypatch, tmp_path, capsys
) -> None:
    asset = tmp_path / "catalog.json"
    original = '{"schema_version":1}\n'
    asset.write_text(original, encoding="utf-8")
    monkeypatch.setattr(producer, "ASSET_PATH", asset)

    def fail(timeout: float = 30.0) -> list[dict]:
        raise OSError("network down")

    monkeypatch.setattr(producer, "fetch_rows", fail)
    assert producer.main([]) != 0
    assert "network down" in capsys.readouterr().err
    assert asset.read_text(encoding="utf-8") == original
