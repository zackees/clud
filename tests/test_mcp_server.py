"""Unit tests for the bundled MCP bridge (`src/clud/mcp_server.py`).

The bridge is opt-in (``clud[mcp]``); these tests skip when the ``mcp`` SDK
is absent. No real clud binary or daemon is exercised — tools that spawn clud
run against a fake ``CLUD_BIN`` script.
"""

from __future__ import annotations

import asyncio
import importlib.util
import json
import sys
from pathlib import Path

import pytest

pytest.importorskip("mcp")

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "src" / "clud" / "mcp_server.py"


@pytest.fixture
def bridge():
    name = "clud_test_mcp_server"
    spec = importlib.util.spec_from_file_location(name, SCRIPT)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    try:
        yield module
    finally:
        sys.modules.pop(name, None)


class _FakeCtx:
    """Duck-typed MCP context: logging is a no-op, never fatal."""

    async def info(self, message: str) -> None:
        del message


def _fake_clud(tmp_path: Path, body: str) -> str:
    script = tmp_path / "fake-clud"
    script.write_text("#!/bin/sh\n" + body, encoding="utf-8")
    script.chmod(0o755)
    return str(script)


def test_build_argv_assembles_backend_model_and_flags(bridge, monkeypatch) -> None:
    monkeypatch.delenv("CLUD_BIN", raising=False)
    assert bridge._build_argv("hi", "claude", "", "") == [
        "clud",
        "-p",
        "hi",
        "--claude",
    ]
    assert bridge._build_argv("hi", "codex", "", "") == [
        "clud",
        "-p",
        "hi",
        "--codex",
    ]
    assert bridge._build_argv("hi", "claude", "terra", "--safe") == [
        "clud",
        "-p",
        "hi",
        "--claude",
        "--model",
        "terra",
        "--safe",
    ]
    monkeypatch.setenv("CLUD_BIN", "/opt/clud")
    assert bridge._build_argv("hi", "claude", "", "")[0] == "/opt/clud"


def test_resolve_cwd_expands_and_absolutizes(bridge, monkeypatch, tmp_path: Path) -> None:
    monkeypatch.chdir(tmp_path)
    assert bridge._resolve_cwd("", "/fallback") == "/fallback"
    monkeypatch.setenv("HOME", str(tmp_path))
    assert bridge._resolve_cwd("~/work", "") == str(tmp_path / "work")
    assert bridge._resolve_cwd("rel/dir", "") == str(tmp_path / "rel" / "dir")
    assert bridge._resolve_cwd(str(tmp_path), "") == str(tmp_path)


def test_extract_text_joins_result_and_assistant_text(bridge) -> None:
    events = [
        {"kind": "raw_jsonl", "data": {"line": json.dumps({"type": "result", "result": "done"})}},
        {
            "kind": "raw_jsonl",
            "data": {
                "line": json.dumps(
                    {
                        "type": "assistant",
                        "message": {
                            "content": [
                                {"type": "text", "text": "part1"},
                                {"type": "tool_use", "name": "Bash"},
                            ]
                        },
                    }
                )
            },
        },
        {"kind": "raw_jsonl", "data": {"line": "not json"}},
        {"kind": "other", "data": {"line": json.dumps({"type": "result", "result": "skipped"})}},
    ]
    assert bridge._extract_text(events) == "done\npart1"


def test_dry_run_returns_the_launch_plan_json(bridge, tmp_path: Path, monkeypatch) -> None:
    fake = _fake_clud(tmp_path, 'echo \'{"model_provider":"claude","dry_run":true}\'\n')
    monkeypatch.setenv("CLUD_BIN", fake)
    result = asyncio.run(
        bridge.dry_run("hello", _FakeCtx(), backend="codex", model="terra")
    )
    plan = json.loads(result)
    assert plan["model_provider"] == "claude"
    assert plan["dry_run"] is True


def test_run_streams_output_and_appends_exit_code(
    bridge, tmp_path: Path, monkeypatch
) -> None:
    fake = _fake_clud(tmp_path, "echo hello-from-clud\n")
    monkeypatch.setenv("CLUD_BIN", fake)
    result = asyncio.run(bridge.run("hello", _FakeCtx()))
    assert "hello-from-clud" in result
    assert "[clud exited with code 0]" in result


def test_run_rejects_unknown_backend(bridge) -> None:
    result = asyncio.run(bridge.run("hello", _FakeCtx(), backend="gpt"))
    assert result.startswith("error: unknown backend")


def test_discover_api_reads_base_url_and_token(bridge, tmp_path: Path, monkeypatch) -> None:
    fake = _fake_clud(
        tmp_path,
        'echo \'{"base_url":"http://127.0.0.1:1","token":"tok"}\'\n',
    )
    monkeypatch.setenv("CLUD_BIN", fake)
    base, token = asyncio.run(bridge._discover_api())
    assert (base, token) == ("http://127.0.0.1:1", "tok")
