"""#1829: DeepSeek Harness as a first-class launch target.

Runs the real binary against an isolated HOME, the debug-only file vault, and
a fake ``dsh`` on PATH that records its argv and the provider variables it
received. No network and no real dsh install are involved: the managed npm
install and live provider calls are covered by the clean-room lanes.
"""

from __future__ import annotations

import json
import os
import platform
from pathlib import Path

import pytest

from .test_mock_agents import _run

pytestmark = [
    pytest.mark.integration,
    pytest.mark.skipif(
        platform.system() == "Windows",
        reason="the fake dsh is a POSIX script; Windows .cmd launch is covered natively",
    ),
]

DEEPSEEK_CANARY = "sk-canary-deepseek-1829"
OPENROUTER_CANARY = "sk-or-canary-openrouter-1829"
# Mirrors provider_auth::test_vault_path for each provider's vault identifiers.
VAULT_FILES = {
    "deepseek": "clud_deepseek--api_key_v1.secret",
    "openrouter": "clud_openrouter--api_key_v1.secret",
}
PROVIDER_VARS = ("DEEPSEEK_API_KEY", "OPENROUTER_API_KEY")

FAKE_DSH = """#!/usr/bin/env python
import json, os, sys
keys = ("DEEPSEEK_API_KEY", "OPENROUTER_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL")
with open(os.environ["FAKE_DSH_RECORD"], "w", encoding="utf-8") as handle:
    json.dump({"argv": sys.argv[1:], "env": {k: os.environ.get(k) for k in keys}}, handle)
print("OK")
"""


def _env(mock_env: dict[str, str], tmp_path: Path, **seed: str) -> dict[str, str]:
    vault = tmp_path / "vault"
    vault.mkdir()
    for provider, secret in seed.items():
        (vault / VAULT_FILES[provider]).write_text(secret, encoding="utf-8")
    fake_dir = tmp_path / "fake-dsh"
    fake_dir.mkdir()
    fake = fake_dir / "dsh"
    fake.write_text(FAKE_DSH, encoding="utf-8")
    fake.chmod(0o755)
    env = mock_env.copy()
    for name in PROVIDER_VARS:
        env.pop(name, None)
    env["PATH"] = str(fake_dir) + os.pathsep + env["PATH"]
    env["CLUD_INTEGRATION_TESTS"] = "1"
    env["CLUD_TEST_SECRET_STORE_DIR"] = str(vault)
    env["FAKE_DSH_RECORD"] = str(tmp_path / "record.json")
    # Never send a canary key to a real provider: an unreachable probe is
    # inconclusive, and clud continues offline.
    env["CLUD_TEST_CREDENTIAL_PROBE_URL"] = "http://127.0.0.1:9/v1/key"
    return env


def _record(env: dict[str, str]) -> dict | None:
    path = Path(env["FAKE_DSH_RECORD"])
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def _dry_run(clud: Path, env: dict[str, str], *args: str) -> dict:
    result = _run(clud, "--dry-run", *args, "-p", "hi", env=env)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def _patch_arg(argv: list[str]) -> str:
    return argv[argv.index("--patch") + 1]


def test_dsh_and_deepseek_spellings_resolve_to_the_same_openrouter_plan(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path)
    short = _dry_run(clud_binary, env, "--openrouter", "--harness", "dsh")
    long = _dry_run(clud_binary, env, "--openrouter", "--harness", "deepseek")
    assert short["command"] == long["command"]
    assert short["command"][1:4] == ["--profile", "headless", "--patch"]
    # A dry run reads no vault and writes no overlay.
    assert not Path(_patch_arg(short["command"])).exists()


def test_openrouter_launch_gets_only_its_key_and_an_overlay(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path, openrouter=OPENROUTER_CANARY, deepseek=DEEPSEEK_CANARY)
    plan = _dry_run(clud_binary, env, "--openrouter", "--harness", "dsh")
    result = _run(clud_binary, "--openrouter", "--harness", "dsh", "-p", "hi", env=env)
    assert result.returncode == 0, result.stderr
    record = _record(env)
    assert record is not None
    assert record["env"]["OPENROUTER_API_KEY"] == OPENROUTER_CANARY
    assert record["env"]["DEEPSEEK_API_KEY"] is None
    assert record["env"]["ANTHROPIC_AUTH_TOKEN"] is None
    assert record["env"]["ANTHROPIC_BASE_URL"] is None
    patch = Path(_patch_arg(record["argv"]))
    body = patch.read_text(encoding="utf-8")
    assert "provider: openrouter" in body
    assert "baseURL: 'https://openrouter.ai/api'" in body
    assert f"model: '{plan['model_selection']['wire_model']}'" in body
    assert OPENROUTER_CANARY not in body
    for stream in (result.stdout, result.stderr, json.dumps(plan)):
        assert OPENROUTER_CANARY not in stream


def test_openrouter_model_flag_reaches_dsh(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path, openrouter=OPENROUTER_CANARY)
    result = _run(
        clud_binary,
        "--openrouter",
        "--harness",
        "dsh",
        "--model",
        "openai/gpt-5.5",
        "-p",
        "hi",
        env=env,
    )
    assert result.returncode == 0, result.stderr
    record = _record(env)
    assert record is not None
    body = Path(_patch_arg(record["argv"])).read_text(encoding="utf-8")
    assert "model: 'openai/gpt-5.5'" in body


def test_deepseek_launch_gets_the_vault_key_and_no_overlay(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path, deepseek=DEEPSEEK_CANARY, openrouter=OPENROUTER_CANARY)
    result = _run(clud_binary, "--deepseek", "--harness", "dsh", "-p", "hi", env=env)
    assert result.returncode == 0, result.stderr
    record = _record(env)
    assert record is not None
    assert record["argv"] == ["--profile", "headless", "hi"]
    assert record["env"]["DEEPSEEK_API_KEY"] == DEEPSEEK_CANARY
    assert record["env"]["OPENROUTER_API_KEY"] is None
    assert DEEPSEEK_CANARY not in result.stdout + result.stderr


def test_an_ambient_key_wins_over_the_vault(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path, deepseek=DEEPSEEK_CANARY)
    env["DEEPSEEK_API_KEY"] = "sk-ambient-1829"
    result = _run(clud_binary, "--deepseek", "--harness", "dsh", "-p", "hi", env=env)
    assert result.returncode == 0, result.stderr
    record = _record(env)
    assert record is not None
    assert record["env"]["DEEPSEEK_API_KEY"] == "sk-ambient-1829"


@pytest.mark.parametrize(
    ("flag", "login"),
    [("--openrouter", "clud auth login openrouter"), ("--deepseek", "clud auth login deepseek")],
)
def test_an_explicit_provider_without_a_key_stops_before_dsh(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path, flag: str, login: str
) -> None:
    env = _env(mock_env, tmp_path)
    result = _run(clud_binary, flag, "--harness", "dsh", "-p", "hi", env=env)
    assert result.returncode != 0
    assert login in result.stderr
    assert _record(env) is None


def test_a_bare_harness_launch_still_runs_on_dsh_owned_credentials(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    env = _env(mock_env, tmp_path)
    result = _run(clud_binary, "--harness", "dsh", "-p", "hi", env=env)
    assert result.returncode == 0, result.stderr
    record = _record(env)
    assert record is not None
    assert record["env"]["DEEPSEEK_API_KEY"] is None
