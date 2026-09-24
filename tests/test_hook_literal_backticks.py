"""Full hook-path regressions for inert backticks in shell arguments."""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

from tests import process


def _binary(name: str) -> Path:
    suffix = ".exe" if sys.platform == "win32" else ""
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name + suffix)
        if clud
        else Path(__file__).resolve().parents[1] / "target/debug" / (name + suffix)
    )
    assert candidate.is_file(), f"build {name} before running process tests"
    return candidate


def test_pretool_hook_allows_literal_backticks_in_shell_arguments(tmp_path: Path) -> None:
    shim_name = "r" + "m" + (".exe" if sys.platform == "win32" else "")
    shim = tmp_path / shim_name
    shutil.copy2(_binary("clud-shim"), shim)
    tick = chr(96)
    pipe = chr(124)
    dollar = chr(36)
    allowed_commands = [
        (
            f"rg -l --glob '*.md' --glob '!projects/**' 'Run {tick}git diff "
            f"@\\{{upstream\\}}\\.\\.\\.HEAD{tick}{pipe}high effort → 8 inline angles|"
            "Phase 0 — Gather the diff' /home/niteris/.claude /home/niteris/dev/fastled/.claude"
        ),
        f"gh issue comment 1298 --body 'literal {tick}code{tick}'",
    ]
    denied_commands = [
        f"printf x\\ #{tick}rm /tmp/victim{tick}",
        f"printf $(echo x)#{tick}rm /tmp/victim{tick}",
        f"printf x\r#{tick}rm /tmp/victim{tick}",
        "unset CLUD_REVIEW_UNSET; printf '%s' "
        + dollar
        + "{CLUD_REVIEW_UNSET:- #"
        + tick
        + "printf nested"
        + tick
        + "}",
        f"env bash -c 'printf ok {tick}printf nested{tick}'",
        f"perl -e 'print {tick}printf nested{tick}'",
        f"cat <<EOF\nprintf ok # {tick}rm /tmp/victim{tick}\nEOF",
        f"gh issue comment 1298 --editor --attach image.png --body 'literal {tick}text{tick}'",
        f"gh issue comment 1298 --web --body 'literal {tick}text{tick}'",
    ]
    env = os.environ.copy()
    env["PATH"] = str(tmp_path)
    env.pop("RIPGREP_CONFIG_PATH", None)
    for should_allow, commands in ((True, allowed_commands), (False, denied_commands)):
        for command in commands:
            payload = json.dumps(
                {
                    "tool_name": "Bash",
                    "cwd": str(tmp_path),
                    "tool_input": {"command": command},
                }
            )
            result = process.run(
                [str(_binary("clud-block-bad-cmd"))],
                env=env,
                input=payload,
                capture_output=True,
                text=True,
                timeout=30,
            )
            assert (result.returncode == 0) is should_allow, (command, result)

    config = tmp_path / "ripgrep.conf"
    config.write_text("--pre=printf nested\n", encoding="utf-8")
    configured_env = env.copy()
    configured_env["RIPGREP_CONFIG_PATH"] = str(config)
    configured_commands = [
        (allowed_commands[0], False),
        (allowed_commands[0].replace("rg -l", "rg --no-config -l", 1), True),
        (
            allowed_commands[0].replace(
                " /home/niteris/.claude", " -- --no-config /home/niteris/.claude", 1
            ),
            False,
        ),
        (allowed_commands[0].replace("rg -l", "rg -g --no-config -l", 1), False),
    ]
    for command, should_allow in configured_commands:
        payload = json.dumps(
            {
                "tool_name": "Bash",
                "cwd": str(tmp_path),
                "tool_input": {"command": command},
            }
        )
        result = process.run(
            [str(_binary("clud-block-bad-cmd"))],
            env=configured_env,
            input=payload,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert (result.returncode == 0) is should_allow, (command, result)
