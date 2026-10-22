"""#1868: redaction policy is visible and owns the request path when enabled."""

import json

from tests.test_hello import _run


def test_free_openrouter_model_enables_redaction_and_selects_gateway() -> None:
    result = _run(
        "--dry-run", "--openrouter", "--model", "nvidia/nemotron-3-ultra-550b-a55b:free"
    )
    assert result.returncode == 0, result.stderr
    route = json.loads(result.stdout)["route"]
    assert route["redaction"]["enabled"] is True
    assert "free" in route["redaction"]["reason"]
    assert "openrouter" in route["redaction"]["patterns"]
    assert route["backend"] == "unified"


def test_paid_model_defaults_off_and_explicit_redaction_selects_gateway() -> None:
    for flags, expected in [((), False), (("--redact-secrets",), True)]:
        result = _run("--dry-run", "--openrouter", *flags)
        assert result.returncode == 0, result.stderr
        route = json.loads(result.stdout)["route"]
        assert route["redaction"]["enabled"] is expected
        if expected:
            assert route["backend"] == "unified"


def test_explicit_disable_overrides_free_model_default() -> None:
    result = _run(
        "--dry-run", "--openrouter", "--model", "nvidia/nemotron-3-ultra-550b-a55b:free",
        "--no-redact-secrets",
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["route"]["redaction"]["enabled"] is False


def test_native_codex_cannot_silently_ignore_explicit_redaction() -> None:
    result = _run("--dry-run", "--codex", "--redact-secrets")
    assert result.returncode == 2
    assert "--unified" in result.stderr


def test_redaction_override_flags_are_mutually_exclusive() -> None:
    result = _run("--dry-run", "--redact-secrets", "--no-redact-secrets")
    assert result.returncode == 2
    assert "--redact-secrets" in result.stderr
    assert "--no-redact-secrets" in result.stderr
