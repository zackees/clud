"""End-to-end coverage for JSON-path payload rules.

Ports CLIProxyAPI's ``requests.payload`` (``config.example.yaml:266-334``): a
rule selects on model (``*`` wildcards) and protocol, and sets (``default`` /
``override``) or removes (``filter``) parameters by dotted JSON path in the
request body the gateway forwards.

The Rust unit tests cover the matcher and path arithmetic; these run the real
binary through its hidden ``__apply-payload-rules`` seam so the process
boundary — stdin document in, rewritten payload out, exit status on a bad rules
file — is exercised too.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

from tests import process


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _clud_binary() -> Path:
    configured = os.environ.get("CLUD_TEST_BINARY")
    if configured:
        return Path(configured)
    candidate = Path(__file__).resolve().parents[1] / "target" / "debug" / _binary_name("clud")
    assert candidate.is_file(), f"clud test binary not found at {candidate}"
    return candidate


def _apply(
    rules: dict[str, Any], payload: dict[str, Any], model: str, protocol: str = "claude"
) -> dict[str, Any]:
    completed = process.run(
        [
            str(_clud_binary()),
            "__apply-payload-rules",
            "--model",
            model,
            "--protocol",
            protocol,
        ],
        input=json.dumps({"rules": rules, "payload": payload}),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout)


def test_default_fills_absent_params_and_override_replaces_them() -> None:
    rules = {
        "default": [
            {
                "models": [{"name": "claude-*", "protocol": "claude"}],
                "params": {"temperature": 0.2, "max_tokens": 4096},
            }
        ],
        "override": [
            {"models": [{"name": "claude-*"}], "params": {"temperature": 0.0}},
        ],
    }
    result = _apply(rules, {"model": "claude-sonnet-4", "temperature": 1.0}, "claude-sonnet-4")
    payload = result["payload"]
    assert payload["max_tokens"] == 4096, "default fills what the caller omitted"
    assert payload["temperature"] == 0.0, "override wins over both"
    assert result["outcome"] == {
        "defaulted": 1,
        "overridden": 1,
        "filtered": 0,
        "problems": [],
        "changed": True,
    }


def test_filter_removes_a_path_and_a_selector_miss_changes_nothing() -> None:
    rules = {
        "filter": [
            {"models": [{"name": "claude-*"}], "params": ["metadata.user_id"]},
        ]
    }
    hit = _apply(
        rules,
        {"model": "claude-opus-4", "metadata": {"user_id": "probe", "keep": 1}},
        "claude-opus-4",
    )
    assert hit["payload"]["metadata"] == {"keep": 1}
    assert hit["outcome"]["filtered"] == 1

    missed = _apply(rules, {"model": "gpt-6-sol", "metadata": {"user_id": "x"}}, "gpt-6-sol")
    assert missed["payload"]["metadata"] == {"user_id": "x"}
    assert missed["outcome"]["changed"] is False


def test_rules_file_is_loaded_from_disk_and_a_typo_is_refused(tmp_path: Path) -> None:
    rules_file = tmp_path / "payload-rules.json"
    rules_file.write_text(
        json.dumps(
            {
                "override": [
                    {"models": [{"name": "gpt-*", "protocol": "codex"}],
                     "params": {"reasoning.effort": "high"}}
                ]
            }
        ),
        encoding="utf-8",
    )
    completed = process.run(
        [
            str(_clud_binary()),
            "__apply-payload-rules",
            "--rules",
            str(rules_file),
            "--model",
            "gpt-6-sol",
            "--protocol",
            "codex",
        ],
        input=json.dumps({"payload": {"reasoning": {"effort": "low"}}}),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["payload"]["reasoning"]["effort"] == "high"

    broken = tmp_path / "broken.json"
    broken.write_text('{"overide": []}', encoding="utf-8")
    refused = process.run(
        [str(_clud_binary()), "__apply-payload-rules", "--rules", str(broken)],
        input="{}",
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert refused.returncode == 2, "a typo in a rule key must not be silently ignored"
    assert "unknown field" in refused.stderr


def test_query_selectors_reach_array_elements_by_field(tmp_path: Path) -> None:
    """gjson query selectors, including upstream's chained all-matches form.

    CLIProxyAPI writes payload paths in gjson syntax and its own filter example
    is ``input.#(type=="additional_tools")#.tools.#(name=="functions")#...``
    (``config.example.yaml:334``); a dotted path alone cannot name an array
    element, so this pins the selector grammar end to end through the binary.
    """
    rules_file = tmp_path / "rules.json"
    rules_file.write_text(
        json.dumps(
            {
                "filter": [
                    {"models": [{"name": "gpt-*", "protocol": "codex"}],
                     "params": [
                         'input.#(type=="additional_tools")#.tools'
                         '.#(name=="functions")#.tools.#(name=="apply_patch")#'
                     ]}
                ],
                "override": [
                    {"models": [{"name": "gpt-*", "protocol": "codex"}],
                     "params": {'tools.#(name=="WebSearch").type': "disabled"}}
                ],
            }
        ),
        encoding="utf-8",
    )
    completed = process.run(
        [
            str(_clud_binary()),
            "__apply-payload-rules",
            "--rules",
            str(rules_file),
            "--model",
            "gpt-6-sol",
            "--protocol",
            "codex",
        ],
        input=json.dumps(
            {
                "payload": {
                    "tools": [{"name": "WebSearch"}, {"name": "apply_patch"}],
                    "input": [
                        {"type": "message"},
                        {
                            "type": "additional_tools",
                            "tools": [
                                {
                                    "name": "functions",
                                    "tools": [
                                        {"name": "apply_patch"},
                                        {"name": "shell"},
                                    ],
                                }
                            ],
                        },
                    ],
                }
            }
        ),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout)
    payload = result["payload"]
    assert payload["input"][1]["tools"][0]["tools"] == [{"name": "shell"}]
    assert payload["input"][0] == {"type": "message"}
    # The filter addresses the nested Codex envelope only; the flat top-level
    # tools array keeps both entries, one of them freshly typed by the override.
    assert payload["tools"] == [
        {"name": "WebSearch", "type": "disabled"},
        {"name": "apply_patch"},
    ]
    assert result["outcome"]["filtered"] == 1
    assert result["outcome"]["overridden"] == 1
    assert result["outcome"]["problems"] == []


def test_a_malformed_selector_in_a_filter_never_fails_the_request() -> None:
    result = _apply(
        {"filter": [{"params": ["tools.#(name)"]}], "override": [{"params": {"temperature": 0.1}}]},
        {"tools": [{"name": "WebSearch"}]},
        "claude-sonnet-4",
    )
    assert result["payload"]["tools"] == [{"name": "WebSearch"}]
    assert result["payload"]["temperature"] == 0.1
    assert result["outcome"]["filtered"] == 0
    assert len(result["outcome"]["problems"]) == 1
    assert "must compare one field" in result["outcome"]["problems"][0]


def test_a_selector_only_write_path_is_refused_when_the_file_loads(tmp_path: Path) -> None:
    """A selector names an element, not a member, so a write needs a trailing key."""
    rules_file = tmp_path / "selector-only.json"
    rules_file.write_text(
        json.dumps({"override": [{"params": {'tools.#(name=="WebSearch")': "x"}}]}),
        encoding="utf-8",
    )
    completed = process.run(
        [str(_clud_binary()), "__apply-payload-rules", "--rules", str(rules_file)],
        input=json.dumps({"payload": {"tools": []}}),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 2
    assert "add the field to set" in completed.stderr
