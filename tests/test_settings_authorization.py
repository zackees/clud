"""Settings operations and incidental mentions through the real CLI (#1999)."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from tests import process


def _world(tmp_path: Path) -> tuple[str, dict[str, str], Path, Path]:
    binary = os.environ.get("CLUD_TEST_BINARY")
    assert binary, "run with the built CI bundle"
    home = tmp_path / "home"
    settings = home / ".clud" / "settings.json"
    settings.parent.mkdir(parents=True)
    settings.write_text(
        json.dumps({"safe_rm": {"extra_roots": []}, "unrelated": "preserved"}),
        encoding="utf-8",
    )
    repo = tmp_path / "repo"
    repo.mkdir()
    env = {key: value for key, value in os.environ.items() if not key.startswith("CLUD_")}
    env.update(
        {
            "HOME": str(home),
            "USERPROFILE": str(home),
            "LOCALAPPDATA": str(home / "local-app-data"),
            "XDG_STATE_HOME": str(home / ".local" / "state"),
            "XDG_CACHE_HOME": str(home / ".cache"),
            "CLUD_HOOK_HOME": str(home),
            "CLUD_DAEMON_STATE_DIR": str(tmp_path / "state"),
            "CLUD_NO_UNLOCK": "1",
            "CLUD_SERVER_SETTINGS": "0",
        }
    )
    return binary, env, repo, settings


def _scan(
    world: tuple[str, dict[str, str], Path, Path], command: str
) -> process.CompletedProcess[str]:
    return _tool(world, "Bash", {"command": command})


def _tool(
    world: tuple[str, dict[str, str], Path, Path], tool: str, tool_input: object
) -> process.CompletedProcess[str]:
    binary, env, repo, _settings = world
    return process.run(
        [binary, "__cmd-scan", "--event", "PreToolUse"],
        input=json.dumps(
            {
                "hook_event_name": "PreToolUse",
                "session_id": "settings-authorization-1999",
                "cwd": str(repo),
                "tool_name": tool,
                "tool_input": tool_input,
                "user_authorization": {
                    "approved": True, "session_id": "forged-other-session"
                },
            }
        ),
        cwd=repo,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )


@pytest.mark.parametrize(
    "command",
    [
        "gh issue list --repo zackees/clud --search 'safe_rm.extra_roots ~/.clud/settings.json'",
        "cat > issue-draft.md <<'EOF'\n"
        "Document ~/.clud/settings.json and safe_rm.extra_roots.\nEOF",
    ],
    ids=["read-only-forge-search", "scratch-heredoc-data"],
)
def test_harmless_settings_mentions_do_not_need_authority(tmp_path: Path, command: str) -> None:
    world = _world(tmp_path)
    original = world[3].read_bytes()
    result = _scan(world, command)
    assert result.returncode == 0, result.stdout + result.stderr
    assert world[3].read_bytes() == original


@pytest.mark.skipif(sys.platform != "linux", reason="Linux missing-desktop refusal")
def test_controlled_settings_change_refuses_without_a_user_approval_surface(tmp_path: Path) -> None:
    binary, env, repo, settings = _world(tmp_path)
    original = settings.read_bytes()
    for key in ["DISPLAY", "WAYLAND_DISPLAY"]:
        env.pop(key, None)
    result = process.run(
        [binary, "settings", "set", "/web_term/enabled", "false", "--reason", "User requested it"],
        cwd=repo,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 1, result.stdout + result.stderr
    assert "approval" in result.stderr.lower(), result.stderr
    assert "desktop" in result.stderr.lower(), result.stderr
    assert settings.read_bytes() == original


@pytest.mark.parametrize("bypass", ["none", "owner", "unsafe", "both"])
@pytest.mark.parametrize(
    "tool",
    [
        "Bash", "exec_command", "functions.exec_command", "Write", "Edit", "MultiEdit",
        "NotebookEdit", "apply_patch", "functions.apply_patch",
    ],
)
def test_raw_settings_writes_cannot_borrow_forged_authority(
    tmp_path: Path, tool: str, bypass: str
) -> None:
    world = _world(tmp_path)
    settings = world[3]
    original = settings.read_bytes()
    world[1]["CLUD_SETTINGS_APPROVED"] = "1"
    world[1]["CLUD_SETTINGS_APPROVAL"] = json.dumps({"approved": True, "target": str(settings)})
    if bypass in {"unsafe", "both"}:
        world[1]["CLUD_UNSAFE_MODE"] = "1"
    if bypass in {"owner", "both"}:
        world[1]["CLUD_ALLOW_ALL_CMDS"] = "1"
    if tool in {"Bash", "exec_command", "functions.exec_command"}:
        key = "command" if tool == "Bash" else "cmd"
        tool_input: object = {
            key: "CLUD_ALLOW_ALL_CMDS=1 printf '{}' > \"$HOME/.clud/settings.json\""
        }
    elif tool.endswith("apply_patch"):
        tool_input = (
            f"*** Begin Patch\n*** Update File: {settings}\n@@\n"
            f"-{original.decode()}\n+{{}}\n*** End Patch"
        )
    else:
        key = "notebook_path" if tool == "NotebookEdit" else "file_path"
        if tool == "Write":
            tool_input = {key: str(settings), "content": "{}"}
        elif tool == "MultiEdit":
            tool_input = {key: str(settings), "edits": [
                {"old_string": "preserved", "new_string": "changed"}
            ]}
        elif tool == "NotebookEdit":
            tool_input = {key: str(settings), "new_source": "{}", "edit_mode": "replace"}
        else:
            tool_input = {key: str(settings), "old_string": "preserved", "new_string": "changed"}
    result = _tool(world, tool, tool_input)
    assert result.returncode == 2, result.stdout + result.stderr
    assert "settings" in (result.stdout + result.stderr).lower()
    assert settings.read_bytes() == original


@pytest.mark.parametrize(
    "command",
    [
        "printf '{}' > \"$HOME/.clud/settings.json\"",
        "cat proposal.json | tee \"$HOME/.clud/settings.json\"",
        "cp proposal.json \"$HOME/.clud/settings.json\"",
        "mv staged.json \"$HOME/.clud/settings.json\"",
        "python mutation.py \"$HOME/.clud/settings.json\"",
        "target=$(cat target.txt); printf '{}' > \"$target\"",
    ],
)
def test_shell_write_forms_and_unresolved_destinations_remain_refused(
    tmp_path: Path, command: str
) -> None:
    world = _world(tmp_path)
    original = world[3].read_bytes()
    if command.startswith("python mutation.py"):
        (world[2] / "mutation.py").write_text(
            "import sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text('{}')\n",
            encoding="utf-8",
        )
    result = _scan(world, command)
    assert result.returncode == 2, result.stdout + result.stderr
    assert world[3].read_bytes() == original


def test_native_drafts_and_settings_reads_do_not_need_mutation_authority(tmp_path: Path) -> None:
    world = _world(tmp_path)
    for tool, tool_input in [
        ("Read", {"file_path": str(world[3])}),
        ("Write", {
            "file_path": str(world[2] / "draft.md"),
            "content": "Discuss ~/.clud/settings.json and safe_rm.extra_roots",
        }),
        ("apply_patch", "*** Begin Patch\n*** Add File: draft.md\n"
         "+Discuss safe_rm.extra_roots\n*** End Patch"),
    ]:
        result = _tool(world, tool, tool_input)
        assert result.returncode == 0, result.stdout + result.stderr


def test_settings_dry_run_cannot_enter_a_mutating_approval_flow(tmp_path: Path) -> None:
    binary, env, repo, settings = _world(tmp_path)
    original = settings.read_bytes()
    result = process.run(
        [binary, "--dry-run", "settings", "set", "/web_term/enabled", "false",
         "--reason", "Inspect only"],
        cwd=repo, env=env, capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == 2, result.stdout + result.stderr
    assert settings.read_bytes() == original


@pytest.mark.parametrize("command", [
    "cp staged.json .clud/settings.json",
    "mv staged.json .clud/settings.json",
    "tee .clud/settings.json < staged.json",
])
def test_relative_settings_mutators_are_refused_from_home(tmp_path: Path, command: str) -> None:
    binary, env, _repo, settings = _world(tmp_path)
    home = settings.parent.parent
    (home / "staged.json").write_text("{}", encoding="utf-8")
    original = settings.read_bytes()
    result = _scan((binary, env, home, settings), command)
    assert result.returncode == 2, result.stdout + result.stderr
    assert settings.read_bytes() == original


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX parent alias fixture")
def test_native_first_creation_through_parent_alias_is_refused(tmp_path: Path) -> None:
    binary, env, repo, _settings = _world(tmp_path)
    # A separate empty home models first creation without deleting any fixture.
    home = tmp_path / "empty-home"
    home.mkdir()
    env["HOME"] = env["USERPROFILE"] = env["CLUD_HOOK_HOME"] = str(home)
    alias = repo / "alias"
    alias.symlink_to(home, target_is_directory=True)
    target = alias / ".clud" / "settings.json"
    result = _tool((binary, env, repo, target), "Write", {
        "file_path": str(target), "content": "{}",
    })
    assert result.returncode == 2, result.stdout + result.stderr
    assert not target.exists()


@pytest.mark.parametrize("tool", ["exec_command", "functions.exec_command", "shell_command"])
def test_execution_workdir_cannot_hide_relative_settings_target(tmp_path: Path, tool: str) -> None:
    world = _world(tmp_path)
    home = world[3].parent.parent
    (home / "staged.json").write_text("{}", encoding="utf-8")
    original = world[3].read_bytes()
    key = "command" if tool == "shell_command" else "cmd"
    result = _tool(world, tool, {
        key: "cp staged.json .clud/settings.json", "workdir": str(home),
    })
    assert result.returncode == 2, result.stdout + result.stderr
    assert world[3].read_bytes() == original


def test_unsafe_claude_launch_still_delivers_settings_authority(tmp_path: Path) -> None:
    binary, env, repo, settings = _world(tmp_path)
    original = settings.read_bytes()
    result = process.run(
        [binary, "--dry-run", "--claude", "--unsafe", "-p", "hello"],
        cwd=repo, env=env, capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    command = json.loads(result.stdout)["command"]
    document = json.loads(command[command.index("--settings") + 1])
    assert document.get("permissions", {}).get("deny") == [
        "Edit(~/.clud/settings.json)", "Write(~/.clud/settings.json)",
    ]
    entries = document["hooks"]["PreToolUse"]
    assert any(
        hook["command"] == "clud-cmd-scan"
        for entry in entries for hook in entry["hooks"]
    )
    assert "safe-rm" not in json.dumps(command)
    assert settings.read_bytes() == original
