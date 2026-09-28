"""Real Codex CLI against a local scripted Responses provider (#1461)."""

from __future__ import annotations

import hashlib
import json
import os
import shlex
import shutil
from pathlib import Path

import pytest

from tests import process
from tests.integration import test_mock_agents as responses_fixture
from tests.shim_env import session_env


@pytest.fixture(autouse=True)
def _require_bosn_codex_opt_in() -> None:
    if os.environ.get("REAL_CODEX_HARNESS_TESTS") != "1":
        pytest.skip("real Codex harness runs only in the dedicated bosn task")


def _trusted_hook_args(command: str) -> list[str]:
    digest = _hook_digest(command)
    return [
        "-c",
        f'hooks.PreToolUse=[{{matcher="Bash",hooks=[{{type="command",command="{command}"}}]}}]',
        "-c",
        f'hooks.state={{"/<session-flags>/config.toml:pre_tool_use:0:0"={{trusted_hash="sha256:{digest}"}}}}',
    ]


def _hook_digest(command: str) -> str:
    vector = {
        "event_name": "pre_tool_use",
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": command, "timeout": 600, "async": False}],
    }
    canonical = json.dumps(vector, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode()).hexdigest()


def _text_stream(index: int, input_tokens: int, cached_tokens: int) -> bytes:
    item = {
        "id": f"msg_{index}",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "hello", "annotations": []}],
    }
    response = {
        "id": f"resp_{index}",
        "object": "response",
        "created_at": 1,
        "model": "gpt-5.6-sol",
        "status": "completed",
        "output": [item],
        "usage": {
            "input_tokens": input_tokens,
            "input_tokens_details": {"cached_tokens": cached_tokens},
            "output_tokens": 1,
            "total_tokens": input_tokens + 1,
        },
    }
    events = [
        {
            "type": "response.created",
            "response": {**response, "status": "in_progress", "output": []},
        },
        {
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {**item, "status": "in_progress", "content": []},
        },
        {"type": "response.output_item.done", "output_index": 0, "item": item},
        {"type": "response.completed", "response": response},
    ]
    return b"".join(
        f'event: {event["type"]}\ndata: {json.dumps(event, separators=(",", ":"))}\n\n'.encode()
        for event in events
    )


def _tool_stream(
    index: int,
    input_tokens: int,
    cached_tokens: int,
    login: bool,
    command: str = "r" + "m -rf build",
) -> bytes:
    code = (
        "const result = await tools.exec_command("
        + json.dumps({"cmd": command, "login": login, "yield_time_ms": 10000})
        + "); text(result.output);"
    )
    item = {
        "id": f"ctc_{index}",
        "type": "custom_tool_call",
        "status": "completed",
        "call_id": f"call_{index}",
        "namespace": "functions",
        "name": "exec",
        "input": code,
    }
    response = {
        "id": f"resp_{index}",
        "object": "response",
        "created_at": 1,
        "model": "gpt-5.6-sol",
        "status": "completed",
        "output": [item],
        "usage": {
            "input_tokens": input_tokens,
            "input_tokens_details": {"cached_tokens": cached_tokens},
            "output_tokens": 1,
            "total_tokens": input_tokens + 1,
        },
    }
    events = [
        {
            "type": "response.created",
            "response": {**response, "status": "in_progress", "output": []},
        },
        {
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {**item, "status": "in_progress", "input": ""},
        },
        {"type": "response.output_item.done", "output_index": 0, "item": item},
        {"type": "response.completed", "response": response},
    ]
    return b"".join(
        f'event: {event["type"]}\ndata: {json.dumps(event, separators=(",", ":"))}\n\n'.encode()
        for event in events
    )


def test_local_provider_runs_without_network_or_real_home(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    cli = shutil.which("codex")
    assert cli is not None
    home = tmp_path / "home"
    config = home / ".codex"
    config.mkdir(parents=True)
    monkeypatch.setattr(responses_fixture, "_responses_sse", _text_stream)
    provider = responses_fixture._FakeResponsesServer()
    try:
        env = os.environ.copy()
        env.update(HOME=str(home), CODEX_HOME=str(config), OPENAI_API_KEY="fixture-key")
        result = process.run(
            [
                cli,
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                'model_provider="fixture"',
                "-c",
                'model="gpt-5.6-sol"',
                "-c",
                f'model_providers.fixture={{name="fixture",base_url="{provider.base_url}/v1",env_key="OPENAI_API_KEY",wire_api="responses"}}',
                "say hello",
            ],
            cwd=str(tmp_path),
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 0, result.stderr
        assert provider.requests
        payload = json.loads(provider.requests[0].partition(b"\r\n\r\n")[2])
        namespaces = payload["input"][0]["tools"]
        names = [tool.get("name") for namespace in namespaces for tool in namespace["tools"]]
        assert "exec" in names, names
    finally:
        provider.close()


@pytest.mark.parametrize("login", [False, True], ids=["non-login", "login"])
def test_scripted_custom_tool_call_reaches_disposable_repo(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, login: bool
) -> None:
    cli = shutil.which("codex")
    assert cli is not None
    repo = tmp_path / "repo"
    repo.mkdir()
    build = repo / "build"
    build.mkdir()
    (build / "item").write_text("disposable", encoding="utf-8")
    home = tmp_path / "home"
    config = home / ".codex"
    config.mkdir(parents=True)
    shim_dir = tmp_path / "bin"
    shim_dir.mkdir()
    suffix = ".exe" if os.name == "nt" else ""
    build_dir = Path(os.environ.get("CLUD_TEST_BINARY", "/build/target/debug/clud")).parent
    shutil.copy2(build_dir / ("clud-shim" + suffix), shim_dir / ("rm" + suffix))
    shutil.copy2(build_dir / ("clud-shim" + suffix), shim_dir / ("safe-" + "r" + "m" + suffix))
    shell_env = tmp_path / "bash-env"
    shell_env.write_text(
        "export PATH=" + shlex.quote(str(shim_dir)) + ":" + "$" + "PATH\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(
        responses_fixture,
        "_responses_sse",
        lambda index, input_tokens, cached_tokens: (
            _tool_stream(index, input_tokens, cached_tokens, login)
            if index == 1
            else _text_stream(index, input_tokens, cached_tokens)
        ),
    )
    provider = responses_fixture._FakeResponsesServer()
    try:
        env = os.environ.copy()
        env.update(
            HOME=str(home),
            CODEX_HOME=str(config),
            OPENAI_API_KEY="fixture-key",
            CLUD_RM_ROOTS=str(repo),
            BASH_ENV=str(shell_env),
            PATH=os.pathsep.join((str(shim_dir), str(build_dir), env["PATH"])),
        )
        env.update(session_env(build_dir / ("clud-shim" + suffix), shim_dir))
        result = process.run(
            [
                cli,
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                'model_provider="fixture"',
                "-c",
                'model="gpt-5.6-sol"',
                "-c",
                f'model_providers.fixture={{name="fixture",base_url="{provider.base_url}/v1",env_key="OPENAI_API_KEY",wire_api="responses"}}',
                *_trusted_hook_args("clud-cmd-scan"),
                "clean build",
            ],
            cwd=str(repo),
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 0, result.stderr
        assert not build.exists(), result.stderr
        assert len(provider.requests) >= 2
        assert "safe-rm -rf build" in result.stderr
        manifests = list((home / ".clud" / "trash").glob("*/.clud-rm.json"))
        assert len(manifests) == 1, manifests
        assert not any("dangerously-bypass-hook-trust" in arg for arg in result.args)
    finally:
        provider.close()


@pytest.mark.parametrize("login", [False, True], ids=["non-login", "login"])
@pytest.mark.parametrize("nounset_opt_out", [False, True], ids=["nounset", "opt-out"])
@pytest.mark.parametrize(
    "agent_command",
    ["r" + "m -rf build", "find . -name '*.tmp' -delete", "./cleanup.sh"],
    ids=["rewrite-rm", "refuse-find-delete", "script-outside-roots"],
)
def test_clud_launches_real_codex_with_trusted_rewrite(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    login: bool,
    nounset_opt_out: bool,
    agent_command: str,
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    build = repo / "build"
    build.mkdir()
    (build / "item").write_text("disposable", encoding="utf-8")
    (build / "keep.tmp").write_text("keep", encoding="utf-8")
    home = tmp_path / "home"
    config = home / ".codex"
    config.mkdir(parents=True)
    cache_probe = home / ".cache" / "probe"
    cache_probe.mkdir(parents=True)
    cleanup = repo / "cleanup.sh"
    cleanup.write_text(
        '#!/bin/sh\n' + 'r' + 'm -rf "$HOME/.cache/probe"\n', encoding="utf-8"
    )
    cleanup.chmod(0o755)
    monkeypatch.setattr(
        responses_fixture,
        "_responses_sse",
        lambda index, input_tokens, cached_tokens: (
            _tool_stream(index, input_tokens, cached_tokens, login, agent_command)
            if index == 1
            else _text_stream(index, input_tokens, cached_tokens)
        ),
    )
    provider = responses_fixture._FakeResponsesServer()
    try:
        env = os.environ.copy()
        build_dir = Path(os.environ.get("CLUD_TEST_BINARY", "/build/target/debug/clud")).parent
        env.update(
            HOME=str(home),
            CODEX_HOME=str(config),
            OPENAI_API_KEY="fixture-key",
            PATH=os.pathsep.join((str(build_dir), env["PATH"])),
            PWD=str(repo),
        )
        if nounset_opt_out:
            env["CLUD_NO_BASH_NOUNSET"] = "1"
        result = process.run(
            [
                str(build_dir / "clud"),
                "--codex",
                "--no-daemon",
                "--subprocess",
                "-p",
                "clean build",
                "--",
                "--skip-git-repo-check",
                "--ephemeral",
                "-c",
                'model_provider="fixture"',
                "-c",
                f'model_providers.fixture={{name="fixture",base_url="{provider.base_url}/v1",env_key="OPENAI_API_KEY",wire_api="responses"}}',
            ],
            cwd=str(repo),
            env=env,
            capture_output=True,
            text=True,
            timeout=40,
        )
        assert result.returncode == 0, result.stderr
        assert len(provider.requests) >= 2
        manifests = list((home / ".clud" / "trash").glob("*/.clud-rm.json"))
        if agent_command == "./cleanup.sh":
            assert not cache_probe.exists(), result.stderr
            assert build.is_dir(), result.stderr
            assert not manifests, manifests
            audit_files = list((home / ".clud" / "state" / "logs" / ("r" + "m")).glob("*.jsonl"))
            assert audit_files, result.stderr
            assert any('"role":"child"' in path.read_text() for path in audit_files)
        elif agent_command.startswith("find "):
            assert (build / "keep.tmp").is_file(), result.stderr
            assert "safe-rm" in result.stderr, result.stderr
            assert not manifests, manifests
        else:
            assert not build.exists(), result.stderr
            assert "safe-rm -rf build" in result.stderr
            assert len(manifests) == 1, manifests
    finally:
        provider.close()


def test_project_rule_can_forbid_rewritten_safe_rm(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    build = repo / "build"
    build.mkdir()
    (build / "item").write_text("keep", encoding="utf-8")
    rules_dir = repo / ".codex" / "rules"
    rules_dir.mkdir(parents=True)
    (rules_dir / "deletion.rules").write_text(
        'prefix_rule(pattern = ["safe-rm"], decision = "forbidden", '
        'justification = "Keep this build directory")\n',
        encoding="utf-8",
    )
    home = tmp_path / "home"
    config = home / ".codex"
    config.mkdir(parents=True)
    (config / "config.toml").write_text(
        f'[projects."{repo}"]\ntrust_level = "trusted"\n', encoding="utf-8"
    )
    monkeypatch.setattr(
        responses_fixture,
        "_responses_sse",
        lambda index, input_tokens, cached_tokens: (
            _tool_stream(index, input_tokens, cached_tokens, False)
            if index == 1
            else _text_stream(index, input_tokens, cached_tokens)
        ),
    )
    provider = responses_fixture._FakeResponsesServer()
    try:
        env = os.environ.copy()
        build_dir = Path(os.environ.get("CLUD_TEST_BINARY", "/build/target/debug/clud")).parent
        env.update(
            HOME=str(home),
            CODEX_HOME=str(config),
            OPENAI_API_KEY="fixture-key",
            PATH=os.pathsep.join((str(build_dir), env["PATH"])),
            PWD=str(repo),
        )
        result = process.run(
            [
                str(build_dir / "clud"),
                "--codex",
                "--no-daemon",
                "--subprocess",
                "-p",
                "clean build",
                "--",
                "--skip-git-repo-check",
                "--ephemeral",
                "-c",
                'model_provider="fixture"',
                "-c",
                f'model_providers.fixture={{name="fixture",base_url="{provider.base_url}/v1",env_key="OPENAI_API_KEY",wire_api="responses"}}',
            ],
            cwd=str(repo),
            env=env,
            capture_output=True,
            text=True,
            timeout=40,
        )
        assert build.is_dir(), result.stderr
        assert not list((home / ".clud" / "trash").glob("*/.clud-rm.json"))
        assert "Keep this build directory" in result.stderr, result.stderr
    finally:
        provider.close()


def test_user_hook_sources_and_trust_survive_clud_launch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    build = repo / "build"
    build.mkdir()
    (build / "item").write_text("disposable", encoding="utf-8")
    home = tmp_path / "home"
    config = home / ".codex"
    config.mkdir(parents=True)
    json_marker = tmp_path / "json-hook-ran"
    toml_marker = tmp_path / "toml-hook-ran"
    json_command = f"touch {shlex.quote(str(json_marker))}"
    toml_command = f"touch {shlex.quote(str(toml_marker))}"
    hooks_path = config / "hooks.json"
    hooks_path.write_text(
        json.dumps({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": json_command}
        ]}]}}),
        encoding="utf-8",
    )
    config_path = config / "config.toml"
    config_path.write_text(
        "\n".join([
            "[[hooks.PreToolUse]]",
            'matcher = "Bash"',
            "[[hooks.PreToolUse.hooks]]",
            'type = "command"',
            f"command = {json.dumps(toml_command)}",
            "",
            f"[hooks.state.{json.dumps(str(hooks_path) + ':pre_tool_use:0:0')}]",
            f'trusted_hash = "sha256:{_hook_digest(json_command)}"',
            f"[hooks.state.{json.dumps(str(config_path) + ':pre_tool_use:0:0')}]",
            f'trusted_hash = "sha256:{_hook_digest(toml_command)}"',
            "",
        ]),
        encoding="utf-8",
    )
    original_json = hooks_path.read_bytes()
    original_toml = config_path.read_bytes()
    monkeypatch.setattr(
        responses_fixture,
        "_responses_sse",
        lambda index, input_tokens, cached_tokens: (
            _tool_stream(index, input_tokens, cached_tokens, False)
            if index == 1
            else _text_stream(index, input_tokens, cached_tokens)
        ),
    )
    provider = responses_fixture._FakeResponsesServer()
    try:
        env = os.environ.copy()
        build_dir = Path(os.environ.get("CLUD_TEST_BINARY", "/build/target/debug/clud")).parent
        env.update(
            HOME=str(home), CODEX_HOME=str(config), OPENAI_API_KEY="fixture-key",
            PATH=os.pathsep.join((str(build_dir), env["PATH"])), PWD=str(repo),
        )
        result = process.run(
            [
                str(build_dir / "clud"), "--codex", "--no-daemon", "--subprocess",
                "-p", "clean build", "--", "--skip-git-repo-check", "--ephemeral",
                "-c", 'model_provider="fixture"',
                "-c",
                (
                    'model_providers.fixture={name="fixture",'
                    f'base_url="{provider.base_url}/v1",'
                    'env_key="OPENAI_API_KEY",wire_api="responses"}'
                ),
            ],
            cwd=str(repo), env=env, capture_output=True, text=True, timeout=40,
        )
        assert result.returncode == 0, result.stderr
        assert json_marker.exists(), result.stderr
        assert toml_marker.exists(), result.stderr
        assert not build.exists(), result.stderr
        assert "safe-rm -rf build" in result.stderr
        assert hooks_path.read_bytes() == original_json
        assert config_path.read_bytes() == original_toml
        assert not any("dangerously-bypass-hook-trust" in arg for arg in result.args)
    finally:
        provider.close()
