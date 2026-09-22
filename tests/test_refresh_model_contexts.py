"""Producer tests for the served OpenRouter model-context map (#1258).

The fixture replays a recorded datasheet payload through the normalizer, so
these tests never touch the network. Failure-path tests pin the producer's
contract with the scheduled workflow: fetch or parse errors exit non-zero
instead of publishing an empty or stale map.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from ci import refresh_model_contexts as producer

FIXTURE = Path(__file__).parent / "fixtures" / "openrouter_models_sample.json"
ASSET = Path(__file__).parent.parent / "crates" / "clud-bin" / "assets" / "server-settings.json"


def _fixture_rows() -> list[dict]:
    return json.loads(FIXTURE.read_text(encoding="utf-8"))["data"]


def _asset_text() -> str:
    return ASSET.read_text(encoding="utf-8")


def test_fixture_replay_normalizes_to_sorted_exact_windows() -> None:
    windows = producer.normalize(_fixture_rows())
    # The bug that motivated #1258: a 1M model the harness would otherwise
    # clamp to its 200k unknown-model default.
    assert windows["xiaomi/mimo-v2.6-flash"] == 1_048_576
    assert windows["openai/gpt-3.5-turbo-0613"] == 4095
    assert windows["openrouter/auto-beta"] == 2_000_000
    assert list(windows) == sorted(windows)
    assert all(isinstance(value, int) for value in windows.values())
    # Deterministic: replaying the same payload yields the same map.
    assert producer.normalize(_fixture_rows()) == windows


def test_normalize_rejects_malformed_rows() -> None:
    valid = {"id": "xiaomi/mimo-v2.6-flash", "context_length": 1_048_576}
    cases: list[dict] = [
        {"id": "xiaomi/mimo-v2.6-flash"},  # missing context_length
        {"id": "xiaomi/mimo-v2.6-flash", "context_length": "1048576"},  # string, not int
        {"id": "xiaomi/mimo-v2.6-flash", "context_length": 1048576.5},  # float
        {"id": "xiaomi/mimo-v2.6-flash", "context_length": -1},  # below range
        {"id": "xiaomi/mimo-v2.6-flash", "context_length": 10_000_001},  # above range
        {"context_length": 1_048_576},  # missing id
        {"id": "", "context_length": 1_048_576},  # empty id
        {"id": "bad id with spaces", "context_length": 1_048_576},  # charset
        {"id": "bad/id!", "context_length": 1_048_576},  # charset
        {"id": "x" * 129, "context_length": 1_048_576},  # too long
    ]
    for row in cases:
        with pytest.raises(producer.RefreshError):
            producer.normalize([row])
    # A non-object row and a duplicate id also fail loudly.
    with pytest.raises(producer.RefreshError):
        producer.normalize(["not-an-object"])
    with pytest.raises(producer.RefreshError):
        producer.normalize([valid, dict(valid)])


def test_rewrite_touches_only_its_section() -> None:
    original = (
        json.dumps(
            {
                "schema_version": 1,
                "sections": {
                    "deepseek": {
                        "default_model": "deepseek-flash",
                        "subagent_model": "deepseek-flash[1m]",
                    },
                    "model_contexts": {"old/model": 123},
                },
            },
            indent=2,
        )
        + "\n"
    )
    updated = producer.rewrite(original, {"new/model": 456})
    document = json.loads(updated)
    assert document["schema_version"] == 1  # additive change: never bumped
    assert document["sections"]["deepseek"] == {
        "default_model": "deepseek-flash",
        "subagent_model": "deepseek-flash[1m]",
    }
    assert document["sections"]["model_contexts"] == {"new/model": 456}
    # Everything before the model_contexts key is byte-for-byte unchanged.
    assert original.split(f'"{producer.SECTION_KEY}"')[0] == updated.split(
        f'"{producer.SECTION_KEY}"'
    )[0]


def test_rewrite_is_idempotent_on_the_committed_document() -> None:
    text = _asset_text()
    windows = json.loads(text)["sections"][producer.SECTION_KEY]
    assert producer.rewrite(text, windows) == text


def test_committed_seed_covers_the_reported_model() -> None:
    document = json.loads(_asset_text())
    assert document["schema_version"] == 1
    windows = document["sections"][producer.SECTION_KEY]
    # The day-one seed from the live datasheet (#1258). If a later refresh
    # changes this value, that is a real window change worth reviewing.
    assert windows["xiaomi/mimo-v2.6-flash"] == 1_048_576
    assert all(
        producer.MIN_CONTEXT_TOKENS <= value <= producer.MAX_CONTEXT_TOKENS
        for value in windows.values()
    )


def test_main_exits_nonzero_when_the_fetch_fails(monkeypatch, tmp_path) -> None:
    asset = tmp_path / "server-settings.json"
    asset.write_text(_asset_text(), encoding="utf-8")
    monkeypatch.setattr(producer, "ASSET_PATH", asset)

    def unreachable(timeout: float = 30.0) -> list[dict]:
        raise OSError("network down")

    monkeypatch.setattr(producer, "fetch_rows", unreachable)
    assert producer.main([]) != 0
    assert asset.read_text(encoding="utf-8") == _asset_text()  # untouched


def test_main_exits_nonzero_on_an_empty_or_malformed_datasheet(monkeypatch, tmp_path) -> None:
    asset = tmp_path / "server-settings.json"
    asset.write_text(_asset_text(), encoding="utf-8")
    monkeypatch.setattr(producer, "ASSET_PATH", asset)

    for payload in ([], [{"id": "x/y", "context_length": None}]):
        monkeypatch.setattr(producer, "fetch_rows", lambda payload=payload: payload)
        assert producer.main([]) != 0
    assert asset.read_text(encoding="utf-8") == _asset_text()  # untouched


def test_main_refreshes_then_reports_no_change(monkeypatch, tmp_path) -> None:
    original = (
        json.dumps(
            {
                "schema_version": 1,
                "sections": {
                    "deepseek": {
                        "default_model": "deepseek-flash",
                        "subagent_model": "deepseek-flash[1m]",
                    }
                },
            },
            indent=2,
        )
        + "\n"
    )
    asset = tmp_path / "server-settings.json"
    asset.write_text(original, encoding="utf-8")
    monkeypatch.setattr(producer, "ASSET_PATH", asset)
    monkeypatch.setattr(producer, "fetch_rows", lambda: _fixture_rows())

    assert producer.main([]) == 0
    written = asset.read_text(encoding="utf-8")
    document = json.loads(written)
    assert document["sections"]["deepseek"]["default_model"] == "deepseek-flash"
    assert document["sections"][producer.SECTION_KEY]["xiaomi/mimo-v2.6-flash"] == 1_048_576
    # Second run against fresh data is a no-op.
    assert producer.main([]) == 0
    assert asset.read_text(encoding="utf-8") == written
