"""#1304: `clud --openrouter --model <id>` becomes the saved default.

Runs the real binary against an isolated HOME and the debug-only file vault,
which is empty, so a live launch stops at the credential preflight with no
network call. The model is saved before that point, so the next plain
`clud --openrouter` reuses it.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from .test_mock_agents import _run

pytestmark = pytest.mark.integration


def _env(mock_env: dict[str, str], tmp_path: Path) -> dict[str, str]:
    vault = tmp_path / "empty-vault"
    vault.mkdir()
    env = mock_env.copy()
    env["CLUD_INTEGRATION_TESTS"] = "1"
    env["CLUD_TEST_SECRET_STORE_DIR"] = str(vault)
    return env


def _saved_openrouter_model(env: dict[str, str]) -> str | None:
    settings = Path(env["HOME"]) / ".clud" / "settings.json"
    if not settings.exists():
        return None
    document = json.loads(settings.read_text(encoding="utf-8"))
    return document.get("providers", {}).get("openrouter", {}).get("model")


def _dry_run_selection(clud: Path, env: dict[str, str], *args: str) -> dict:
    result = _run(clud, "--dry-run", "--openrouter", *args, "-p", "hi", env=env)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)["model_selection"]


def test_an_explicit_openrouter_model_is_saved_and_reused(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path)

    # A dry run resolves the model but never writes it.
    assert _dry_run_selection(clud_binary, env, "--model", "openai/gpt-5.5")[
        "wire_model"
    ] == "openai/gpt-5.5"
    assert _saved_openrouter_model(env) is None

    live = _run(
        clud_binary, "--openrouter", "--model", "openai/gpt-5.5", "-p", "hi", env=env
    )
    # No key is stored, so the launch stops at the preflight -- after saving.
    assert live.returncode == 2, live.stderr
    assert "clud auth login openrouter" in live.stderr
    assert "[clud] saved OpenRouter model openai/gpt-5.5 as the default" in live.stderr
    assert _saved_openrouter_model(env) == "openai/gpt-5.5"

    selection = _dry_run_selection(clud_binary, env)
    assert selection["wire_model"] == "openai/gpt-5.5"
    assert selection["model_source"] == "provider_setting"

    # An explicit --model wins over the saved one and replaces it.
    assert _dry_run_selection(clud_binary, env, "--model", "x-ai/grok-5")[
        "wire_model"
    ] == "x-ai/grok-5"
    live = _run(clud_binary, "--openrouter", "--model", "x-ai/grok-5", "-p", "hi", env=env)
    assert live.returncode == 2, live.stderr
    assert _saved_openrouter_model(env) == "x-ai/grok-5"

    # Re-selecting the saved model is not announced again.
    again = _run(clud_binary, "--openrouter", "--model", "x-ai/grok-5", "-p", "hi", env=env)
    assert "saved OpenRouter model" not in again.stderr


def test_other_providers_never_save_a_model(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path)
    live = _run(clud_binary, "--deepseek", "--model", "deepseek-flash", "-p", "hi", env=env)
    assert live.returncode == 2, live.stderr
    assert "saved" not in live.stderr
    settings = Path(env["HOME"]) / ".clud" / "settings.json"
    if settings.exists():
        document = json.loads(settings.read_text(encoding="utf-8"))
        assert "model" not in document.get("providers", {}).get("deepseek", {})
