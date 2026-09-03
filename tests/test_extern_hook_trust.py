"""End-to-end coverage for Phase 4: extern trust + the #966 §6 firing matrix.

zackees/clud#967 Phase 4. The Rust tests cover the decision tables; these
check the parts only a real process exercises:

- an untrusted extern checkout's own hooks stay off, with one visible notice
  naming `clud extern trust <name>` (D9);
- trusting the checkout turns its hooks on, and a re-clone from a different
  origin (or a GC teardown followed by a re-clone) does not carry the trust;
- a declared child's own hooks run rooted at the child, layered after the
  parent's — any deny denies, parent first;
- an extern hook runs rooted at the checkout itself;
- the `clud extern trust` CLI records, lists, and revokes;
- a codex session (no CLAUDE_PROJECT_DIR) gates sub-repo hook execution on
  `~/.codex/config.toml` project trust.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

from tests import process

BLOCKING_HOOK = "import sys\nsys.exit(2)\n"


def _binary_name(name: str) -> str:
    return f"{name}.exe" if sys.platform == "win32" else name


def _cmd_scan_binary() -> Path:
    clud_binary = os.environ.get("CLUD_TEST_BINARY")
    if clud_binary:
        sibling = Path(clud_binary).with_name(_binary_name("clud-cmd-scan"))
        if sibling.is_file():
            return sibling

    resolved = shutil.which(_binary_name("clud-cmd-scan"))
    if resolved:
        return Path(resolved)

    raise AssertionError("clud-cmd-scan test binary not found")


def _clud_binary() -> Path:
    clud_binary = os.environ.get("CLUD_TEST_BINARY")
    if clud_binary and Path(clud_binary).is_file():
        return Path(clud_binary)

    resolved = shutil.which(_binary_name("clud"))
    if resolved:
        return Path(resolved)

    raise AssertionError("clud test binary not found")


def _python_hook(script: Path, body: str) -> str:
    script.write_text(body, encoding="utf-8")
    # Forward slashes so Windows separators are not read as escapes by the
    # shell that runs the hook command.
    return f'"{sys.executable}" "{script.as_posix()}"'


def _make_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    return repo


def _make_extern(repo: Path, name: str, origin: str) -> Path:
    """A GC-tracked extern checkout (legacy in-tree layout, still recognized).

    The `.git/config` carries the origin URL that the trust key defends
    against; clud reads it directly, no git spawn.
    """
    extern = repo / ".extern-repos" / name
    (extern / ".git").mkdir(parents=True)
    (extern / ".git" / "config").write_text(
        f'[remote "origin"]\n\turl = {origin}\n', encoding="utf-8"
    )
    return extern


def _frontend_hooks(repo_root: Path, hooks: dict) -> None:
    """Declare hooks in the repo's `.claude/settings.json` — the Tier B
    source for a sub-repo that has not opted into `.clud/hooks.json`."""
    claude = repo_root / ".claude"
    claude.mkdir(exist_ok=True)
    (claude / "settings.json").write_text(json.dumps({"hooks": hooks}), encoding="utf-8")


def _run(
    tmp_path: Path,
    *,
    payload: dict,
    claude: bool = True,
    home: Path | None = None,
    argv_extra: list[str] | None = None,
) -> process.CompletedProcess[str]:
    """Invoke cmd-scan with `payload` on stdin, as the harness would.

    `claude=True` exports CLAUDE_PROJECT_DIR, the harness's project dir that
    a real Claude session always sets; `claude=False` leaves it absent, which
    is how a codex session presents.
    """
    home = home or (tmp_path / "home")
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env.pop("CLUD_BAD_CMD_OVERRIDE", None)
    if claude:
        env["CLAUDE_PROJECT_DIR"] = str(payload.get("cwd") or tmp_path / "repo")
    else:
        env.pop("CLAUDE_PROJECT_DIR", None)
    argv = [str(_cmd_scan_binary()), *(argv_extra or [])]
    return process.run(
        argv,
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )


def _trust_cli(tmp_path: Path, repo: Path, *args: str) -> process.CompletedProcess[str]:
    """Run `clud extern trust ...` from inside the parent repo."""
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    return process.run(
        [str(_clud_binary()), "extern", "trust", *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(repo),
        timeout=60,
    )


def _edit_in_extern(repo: Path, extern: Path) -> dict:
    # A subagent editing a foreign checkout reports cwd at the parent root —
    # containment has to come from the file_path, never from cwd alone.
    return {
        "tool_name": "Edit",
        "cwd": str(repo),
        "tool_input": {"file_path": str(extern / "src.rs")},
    }


# ---------------------------------------------------------------------
# Trust lifecycle (D9): off until named, on after, keyed to name + origin.
# ---------------------------------------------------------------------


def test_untrusted_extern_hooks_stay_off_with_a_visible_notice(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    extern = _make_extern(repo, "dep", "https://example.com/dep.git")
    marker = tmp_path / "ran.txt"
    _frontend_hooks(
        extern,
        {"PreToolUse": [{"command": _python_hook(
            tmp_path / "guard.py", f'open(r"{marker.as_posix()}", "w").write("ran")\n'
        )}]},
    )

    result = _run(tmp_path, payload=_edit_in_extern(repo, extern))

    assert result.returncode == 0, result.stdout + result.stderr
    assert not marker.exists(), "an untrusted checkout's hooks must not run"
    assert "clud extern trust dep" in result.stderr, result.stderr


def test_trusting_the_checkout_turns_its_hooks_on(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    extern = _make_extern(repo, "dep", "https://example.com/dep.git")
    marker = tmp_path / "ran.txt"
    _frontend_hooks(
        extern,
        {"PreToolUse": [{"command": _python_hook(
            tmp_path / "guard.py",
            f'open(r"{marker.as_posix()}", "w").write("ran")\n' + BLOCKING_HOOK,
        )}]},
    )

    trust = _trust_cli(tmp_path, repo, "dep")
    assert trust.returncode == 0, trust.stderr
    assert 'trusted extern checkout "dep"' in trust.stdout, trust.stdout

    result = _run(tmp_path, payload=_edit_in_extern(repo, extern))

    assert result.returncode == 2, "a trusted guard may block"
    assert marker.read_text(encoding="utf-8") == "ran"


def test_a_reclone_from_a_different_origin_does_not_carry_trust(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    extern = _make_extern(repo, "dep", "https://example.com/dep.git")
    marker = tmp_path / "ran.txt"
    hooks = {"PreToolUse": [{"command": _python_hook(
        tmp_path / "guard.py", f'open(r"{marker.as_posix()}", "w").write("ran")\n'
    )}]}
    _frontend_hooks(extern, hooks)
    assert _trust_cli(tmp_path, repo, "dep").returncode == 0

    # Re-clone: same checkout name, a different remote.
    (extern / ".git" / "config").write_text(
        '[remote "origin"]\n\turl = https://evil.example.com/dep.git\n', encoding="utf-8"
    )
    result = _run(tmp_path, payload=_edit_in_extern(repo, extern))

    assert result.returncode == 0, result.stdout + result.stderr
    assert not marker.exists(), "trust is keyed to the origin, so it must not carry"
    assert "clud extern trust dep" in result.stderr


def test_a_stale_entry_after_gc_teardown_is_harmless(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    extern = _make_extern(repo, "dep", "https://example.com/dep.git")
    marker = tmp_path / "ran.txt"
    hooks = {"PreToolUse": [{"command": _python_hook(
        tmp_path / "guard.py",
        f'open(r"{marker.as_posix()}", "w").write("ran")\n' + BLOCKING_HOOK,
    )}]}
    _frontend_hooks(extern, hooks)
    assert _trust_cli(tmp_path, repo, "dep").returncode == 0

    # GC tears the checkout down; the trust entry stays recorded.
    shutil.rmtree(extern)

    # A fresh checkout from a different remote must not inherit the trust.
    re_cloned = _make_extern(repo, "dep", "https://other.example.com/dep.git")
    _frontend_hooks(re_cloned, hooks)
    result = _run(tmp_path, payload=_edit_in_extern(repo, re_cloned))
    assert result.returncode == 0, result.stdout + result.stderr
    assert not marker.exists()
    assert "clud extern trust dep" in result.stderr

    # A fresh checkout from the same remote is still trusted — the user
    # trusted that name + remote, not one directory generation.
    shutil.rmtree(re_cloned)
    restored = _make_extern(repo, "dep", "https://example.com/dep.git")
    _frontend_hooks(restored, hooks)
    result = _run(tmp_path, payload=_edit_in_extern(repo, restored))
    assert result.returncode == 2, result.stdout + result.stderr
    assert marker.read_text(encoding="utf-8") == "ran"


def test_an_opted_in_extern_checkout_is_trust_gated_too(tmp_path: Path) -> None:
    # `discover` (`.clud/hooks.json`) is preferred over the frontend source,
    # but the trust gate applies whichever source the hooks came from.
    repo = _make_repo(tmp_path)
    extern = _make_extern(repo, "dep", "https://example.com/dep.git")
    marker = tmp_path / "ran.txt"
    (extern / ".clud").mkdir()
    (extern / ".clud" / "hooks.json").write_text(
        json.dumps({"hooks": {"PreToolUse": [{"command": _python_hook(
            tmp_path / "guard.py",
            f'open(r"{marker.as_posix()}", "w").write("ran")\n' + BLOCKING_HOOK,
        )}]}}),
        encoding="utf-8",
    )

    result = _run(tmp_path, payload=_edit_in_extern(repo, extern))
    assert result.returncode == 0
    assert not marker.exists()
    assert "clud extern trust dep" in result.stderr

    assert _trust_cli(tmp_path, repo, "dep").returncode == 0
    result = _run(tmp_path, payload=_edit_in_extern(repo, extern))
    assert result.returncode == 2
    assert marker.read_text(encoding="utf-8") == "ran"


# ---------------------------------------------------------------------
# Rooting: an extern hook runs rooted at the checkout.
# ---------------------------------------------------------------------


def test_an_extern_hook_runs_rooted_at_the_checkout(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    extern = _make_extern(repo, "dep", "https://example.com/dep.git")
    observed = tmp_path / "observed.txt"
    _frontend_hooks(
        extern,
        {"PreToolUse": [{"command": _python_hook(
            tmp_path / "observe.py",
            "import os\n"
            f'open(r"{observed.as_posix()}", "w").write('
            'os.getcwd() + "|" + os.environ.get("CLUD_PROJECT_DIR", ""))\n',
        )}]},
    )
    assert _trust_cli(tmp_path, repo, "dep").returncode == 0

    result = _run(tmp_path, payload=_edit_in_extern(repo, extern))

    assert result.returncode == 0, result.stdout + result.stderr
    cwd, project_dir = observed.read_text(encoding="utf-8").split("|")
    assert Path(cwd).resolve() == extern.resolve(), cwd
    assert Path(project_dir).resolve() == extern.resolve(), project_dir


# ---------------------------------------------------------------------
# Layered deny: parent first, then child; any deny denies (D6/D7).
# ---------------------------------------------------------------------


def _make_child(repo: Path) -> Path:
    child = repo / "packages" / "core"
    (child / ".git").mkdir(parents=True)
    (repo / ".clud").mkdir(exist_ok=True)
    (repo / ".clud" / "settings.json").write_text(
        json.dumps({"hook_roots": {"children": ["packages/core"]}}), encoding="utf-8"
    )
    return child


def _append_hook(script: Path, order: Path, text: str, block: bool) -> str:
    body = (
        f'open(r"{order.as_posix()}", "a").write({text!r})\n'
        + (BLOCKING_HOOK if block else "")
    )
    return _python_hook(script, body)


def test_layered_deny_runs_parent_first_and_stops_at_the_first_block(
    tmp_path: Path,
) -> None:
    repo = _make_repo(tmp_path)
    order = tmp_path / "order.txt"
    parent_hook = _append_hook(tmp_path / "parent.py", order, "parent\n", block=True)
    child_hook = _append_hook(tmp_path / "child.py", order, "child\n", block=False)
    child = _make_child(repo)

    # The parent's own hooks come from `.clud/hooks.json` (Phase 2 D3); the
    # child's, not opted in, come from its frontend settings (Phase 4 D4).
    (repo / ".clud" / "hooks.json").write_text(
        json.dumps({"hooks": {"PreToolUse": [{"command": parent_hook}]}}),
        encoding="utf-8",
    )
    _frontend_hooks(child, {"PreToolUse": [{"command": child_hook}]})

    result = _run(
        tmp_path,
        payload={
            "tool_name": "Edit",
            "cwd": str(repo),
            "tool_input": {"file_path": str(child / "lib.rs")},
        },
    )

    assert result.returncode == 2, result.stdout + result.stderr
    assert (
        order.read_text(encoding="utf-8") == "parent\n"
    ), "the parent's deny must stop the call before the child's hooks run"


def test_layered_deny_child_blocks_when_the_parent_allows(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    order = tmp_path / "order.txt"
    parent_hook = _append_hook(tmp_path / "parent.py", order, "parent\n", block=False)
    child_hook = _append_hook(tmp_path / "child.py", order, "child\n", block=True)
    child = _make_child(repo)

    (repo / ".clud" / "hooks.json").write_text(
        json.dumps({"hooks": {"PreToolUse": [{"command": parent_hook}]}}),
        encoding="utf-8",
    )
    _frontend_hooks(child, {"PreToolUse": [{"command": child_hook}]})

    result = _run(
        tmp_path,
        payload={
            "tool_name": "Edit",
            "cwd": str(repo),
            "tool_input": {"file_path": str(child / "lib.rs")},
        },
    )

    assert result.returncode == 2, result.stdout + result.stderr
    assert (
        order.read_text(encoding="utf-8") == "parent\nchild\n"
    ), "both layers run, in order, when neither denies"
    assert "child" in result.stderr or "Blocked by the project hook" in result.stderr


def test_child_hooks_fire_in_a_codex_session_only_when_the_project_is_trusted(
    tmp_path: Path,
) -> None:
    repo = _make_repo(tmp_path)
    marker = tmp_path / "ran.txt"
    child = _make_child(repo)
    _frontend_hooks(
        child,
        {"PreToolUse": [{"command": _python_hook(
            tmp_path / "guard.py",
            f'open(r"{marker.as_posix()}", "w").write("ran")\n' + BLOCKING_HOOK,
        )}]},
    )
    home = tmp_path / "codex-home"
    payload = {
        "tool_name": "Edit",
        "cwd": str(repo),
        "tool_input": {"file_path": str(child / "lib.rs")},
    }

    # A codex session (no CLAUDE_PROJECT_DIR) whose project is not trusted:
    # the child's own hooks stay off, with a notice.
    result = _run(tmp_path, payload=payload, claude=False, home=home)
    assert result.returncode == 0, result.stdout + result.stderr
    assert not marker.exists(), "untrusted codex projects do not get sub-repo hooks"
    assert "codex project is not trusted" in result.stderr, result.stderr

    # Trusting the project in ~/.codex/config.toml turns them on.
    config = home / ".codex" / "config.toml"
    config.parent.mkdir(parents=True)
    # A TOML *literal* key (single quotes). A Windows key is
    # `c:\\users\\...`, and in a basic (double-quoted) string those
    # backslashes are escapes -- `\\u` in `\\users` starts a unicode escape and
    # the file does not parse, so the project never reads as trusted and the
    # assertion below fails with clud's "not trusted yet" notice.
    config.write_text(
        f"[projects.'{_codex_project_key(str(repo))}']\ntrust_level = \"trusted\"\n",
        encoding="utf-8",
    )
    result = _run(tmp_path, payload=payload, claude=False, home=home)
    assert result.returncode == 2, result.stdout + result.stderr
    assert marker.read_text(encoding="utf-8") == "ran"


def _codex_project_key(path: str) -> str:
    """Mirror codex_trust::normalize_project_path_key for the test's key."""
    s = path.replace("/", "\\")
    if s.startswith(r"\\?\\"):
        s = s[4:]
    looks_windows = (len(s) >= 2 and s[1] == ":") or s.startswith(r"\\?\\")
    return s.lower() if looks_windows else path


# ---------------------------------------------------------------------
# `clud extern trust` CLI lifecycle.
# ---------------------------------------------------------------------


def test_trust_cli_records_lists_and_revokes(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    _make_extern(repo, "dep", "https://example.com/dep.git")

    listed = _trust_cli(tmp_path, repo, "--list")
    assert listed.returncode == 0
    assert "no trust entries recorded" in listed.stdout

    trust = _trust_cli(tmp_path, repo, "dep")
    assert trust.returncode == 0, trust.stderr
    assert 'trusted extern checkout "dep"' in trust.stdout

    listed = _trust_cli(tmp_path, repo, "--list")
    assert "dep\thttps://example.com/dep.git" in listed.stdout

    # Trusting requires a checkout that exists.
    missing = _trust_cli(tmp_path, repo, "nope")
    assert missing.returncode == 1
    assert "no extern checkout named" in missing.stderr

    revoke = _trust_cli(tmp_path, repo, "dep", "--revoke")
    assert revoke.returncode == 0, revoke.stderr
    assert "removed trust for extern checkout" in revoke.stdout

    revoke_again = _trust_cli(tmp_path, repo, "dep", "--revoke")
    assert revoke_again.returncode == 1
    assert "no trust entry" in revoke_again.stderr

    listed = _trust_cli(tmp_path, repo, "--list")
    assert "no trust entries recorded" in listed.stdout


def test_untrusted_hooks_do_not_run_without_a_readable_origin(tmp_path: Path) -> None:
    # A checkout with no origin cannot be re-cloned from a different remote,
    # so it can never be trusted — the gate stays shut and the notice still
    # names the command, which will explain itself.
    repo = _make_repo(tmp_path)
    extern = repo / ".extern-repos" / "dep"
    (extern / ".git").mkdir(parents=True)
    marker = tmp_path / "ran.txt"
    _frontend_hooks(
        extern,
        {"PreToolUse": [{"command": _python_hook(
            tmp_path / "guard.py", f'open(r"{marker.as_posix()}", "w").write("ran")\n'
        )}]},
    )

    result = _run(tmp_path, payload=_edit_in_extern(repo, extern))

    assert result.returncode == 0, result.stdout + result.stderr
    assert not marker.exists()
    assert "clud extern trust dep" in result.stderr
