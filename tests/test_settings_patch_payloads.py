"""Native file-tool regression matrix for the settings guard (#2289)."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from tests import process
from tests.test_settings_authorization import _tool, _world


def _patch(*targets: str, operation: str = "Update") -> str:
    sections = []
    for target in targets:
        body = "" if operation == "Delete" else "+after\n"
        if operation == "Update":
            body = "@@\n-before\n" + body
        sections.append(f"*** {operation} File: {target}\n{body}")
    return "*** Begin Patch\n" + "".join(sections) + "*** End Patch"


def test_codex_bridge_command_envelope_allows_memory_patch(tmp_path: Path) -> None:
    """The live functions.apply_patch refusal logged patch text as command (#2289)."""
    world = _world(tmp_path)
    paths = ["memories/MEMORY.md", "profiles/example/memories/MEMORY.md"]
    for name in paths:
        file = world[2] / name
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text("before\n", encoding="utf-8")
    result = _tool(world, "functions.apply_patch", {"command": _patch(*paths)})
    assert result.returncode == 0, result.stdout + result.stderr


@pytest.mark.parametrize("form", ["raw", "patch", "input", "command", "script"])
@pytest.mark.parametrize("operation", ["Add", "Update", "Delete", "Move"])
def test_patch_envelopes_and_ordinary_operations(
    tmp_path: Path, form: str, operation: str
) -> None:
    world = _world(tmp_path)
    target = world[2] / "profiles" / "Zoë notes" / "memories" / "MEMORY.md"
    if operation == "Move":
        patch = _patch(str(target)).replace("@@\n", "*** Move to: renamed notes.md\n@@\n")
    else:
        patch = _patch(str(target), ".clud/settings.json", operation=operation)
    patch = patch.replace("+after", "+Discuss ~/.clud/settings.json and restart commands as data")
    result = _tool(world, "functions.apply_patch", patch if form == "raw" else {form: patch})
    assert result.returncode == 0, result.stdout + result.stderr


@pytest.mark.parametrize("operation", ["Add", "Update", "Delete", "MoveFrom", "MoveTo", "Mixed"])
@pytest.mark.parametrize("form", ["raw", "patch", "input", "command", "script"])
def test_all_protected_patch_targets_refused_before_any_changes(
    tmp_path: Path, operation: str, form: str
) -> None:
    world = _world(tmp_path)
    original = world[3].read_bytes()
    ordinary = world[2] / "memory.md"
    ordinary.write_text("before\n", encoding="utf-8")
    if operation == "MoveFrom":
        patch = _patch(str(world[3])).replace("@@\n", "*** Move to: memory.md\n@@\n")
    elif operation == "MoveTo":
        patch = _patch("memory.md").replace("@@\n", f"*** Move to: {world[3]}\n@@\n")
    elif operation == "Mixed":
        patch = _patch("memory.md", str(world[3]))
    else:
        patch = _patch(str(world[3]), operation=operation)
    result = _tool(world, "apply_patch", patch if form == "raw" else {form: patch})
    assert result.returncode == 2, result.stdout + result.stderr
    assert "raw writes to user settings" in result.stderr
    assert world[3].read_bytes() == original
    assert ordinary.read_text(encoding="utf-8") == "before\n"


@pytest.mark.parametrize("value", [None, {}, {"patch": None}, {"input": 4}, {"patch": []},
                                     {"arguments": "PRIVATE_SENTINEL"},
                                     {"patch": "PRIVATE_SENTINEL", "command": _patch("memory.md")}])
def test_unsupported_or_ambiguous_inputs_have_truthful_private_diagnostics(
    tmp_path: Path, value: object
) -> None:
    result = _tool(_world(tmp_path), "apply_patch", value)
    assert result.returncode == 2
    assert "patch input" in result.stderr.lower()
    assert "raw writes to user settings" not in result.stderr
    assert "PRIVATE_SENTINEL" not in result.stdout + result.stderr


@pytest.mark.parametrize("patch", [
    "PRIVATE_SENTINEL", "*** Begin Patch\n*** End Patch",
    "*** Begin Patch\n*** Update File:",
    "*** Begin Patch\n*** Update File: memory.md\n@@\n+PRIVATE_SENTINEL",
    "*** Begin Patch\n*** Add File: \n+x\n*** End Patch",
    "*** Begin Patch\n*** Delete File: memory.md\n*** Move to: other.md\n*** End Patch",
    "*** Begin Patch\n*** Update File: memory.md\n*** Move to: first\n"
    "*** Move to: second\n*** End Patch",
    "*** Begin Patch\n*** Add File: memory.md\n+x\n*** Update File:\n*** End Patch",
    "*** Begin Patch\\n*** Update File: memory.md\\n@@\\n+PRIVATE_SENTINEL\\n*** End Patch",
])
def test_malformed_patches_fail_closed_without_claiming_protected_target(
    tmp_path: Path, patch: str
) -> None:
    result = _tool(_world(tmp_path), "apply_patch", {"command": patch})
    assert result.returncode == 2
    assert "malformed patch" in result.stderr.lower()
    assert "raw writes to user settings" not in result.stderr
    assert "PRIVATE_SENTINEL" not in result.stdout + result.stderr


@pytest.mark.parametrize("spelling", ["relative", "parent", "absolute", "home"])
def test_protected_path_spellings(tmp_path: Path, spelling: str) -> None:
    binary, env, repo, settings = _world(tmp_path)
    paths = {"relative": ".clud/settings.json", "parent": "../home/.clud/settings.json",
             "absolute": str(settings), "home": "~/.clud/settings.json"}
    cwd = settings.parent.parent if spelling == "relative" else repo
    original = settings.read_bytes()
    result = _tool(
        (binary, env, cwd, settings), "apply_patch", {"command": _patch(paths[spelling])}
    )
    assert result.returncode == 2
    assert "raw writes to user settings" in result.stderr
    assert settings.read_bytes() == original


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX symlink fixture; no Windows privilege")
@pytest.mark.parametrize("existing", [True, False])
@pytest.mark.parametrize("alias_kind", ["parent", "file"])
def test_symlink_aliases_protect_existing_and_first_creation(
    tmp_path: Path, existing: bool, alias_kind: str
) -> None:
    binary, env, repo, settings = _world(tmp_path)
    if not existing:
        home = tmp_path / "new-home"
        home.mkdir()
        env["HOME"] = env["USERPROFILE"] = env["CLUD_HOOK_HOME"] = str(home)
        settings = home / ".clud/settings.json"
    alias = repo / "alias"
    if alias_kind == "parent":
        alias.symlink_to(settings.parent.parent, target_is_directory=True)
        target = alias / ".clud/settings.json"
    else:
        alias.symlink_to(settings)
        target = alias
    original = settings.read_bytes() if existing else None
    result = _tool((binary, env, repo, settings), "apply_patch", {"command": _patch(str(target))})
    assert result.returncode == 2, result.stdout + result.stderr
    assert "raw writes to user settings" in result.stderr
    if existing:
        assert settings.read_bytes() == original
    else:
        assert not settings.exists()


@pytest.mark.parametrize("tool", ["Write", "Edit", "MultiEdit", "NotebookEdit", "functions.edit"])
@pytest.mark.parametrize("kind", ["ordinary", "protected", "missing"])
def test_other_native_writers(tmp_path: Path, tool: str, kind: str) -> None:
    world = _world(tmp_path)
    key = "notebook_path" if tool == "NotebookEdit" else "file_path"
    target = str(world[3]) if kind == "protected" else "memory.md"
    value = {} if kind == "missing" else {key: target}
    original = world[3].read_bytes()
    result = _tool(world, tool, value)
    assert result.returncode == (0 if kind == "ordinary" else 2), result.stdout + result.stderr
    if kind == "missing":
        assert "target path" in result.stderr.lower()
        assert "raw writes to user settings" not in result.stderr
    assert world[3].read_bytes() == original


def test_camelcase_hook_envelope_and_unknown_tools(tmp_path: Path) -> None:
    binary, env, repo, _ = _world(tmp_path)
    result = process.run(
        [binary, "__cmd-scan", "--event", "PreToolUse"],
        input=json.dumps({"toolName": "functions.apply_patch", "cwdPath": str(repo),
                          "toolInput": {"command": _patch("memories/MEMORY.md")}}),
        cwd=repo, env=env, capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert _tool((binary, env, repo, repo / "unused"), "unknown", None).returncode == 0
