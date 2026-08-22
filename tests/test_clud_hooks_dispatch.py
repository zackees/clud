"""End-to-end coverage for `.clud/hooks.json` dispatch through cmd-scan.

zackees/clud#967 Phase 2. The Rust tests cover the decision table; these check
the part only a real process exercises — that a declared hook is discovered
from the payload's cwd, runs rooted at the repo that declared it even when the
session cwd has drifted, and can block the tool call.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

from tests import process


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


def _python_hook(script: Path, body: str) -> str:
    script.write_text(body, encoding="utf-8")
    # Forward slashes so Windows separators are not read as escapes by the
    # shell that runs the hook command.
    return f'"{sys.executable}" "{script.as_posix()}"'


def _make_repo(tmp_path: Path, hooks: dict) -> Path:
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    (repo / "sub").mkdir()
    clud = repo / ".clud"
    clud.mkdir()
    (clud / "hooks.json").write_text(json.dumps({"hooks": hooks}), encoding="utf-8")
    return repo


def _run(
    tmp_path: Path,
    *,
    payload: dict,
    argv_extra: list[str] | None = None,
) -> process.CompletedProcess[str]:
    """Invoke cmd-scan with `payload` on stdin.

    The repo is identified by the payload's own `cwd`, exactly as the harness
    reports it -- which is the point: discovery has to work from the payload,
    not from the process's own directory.
    """
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    # The dispatcher prefers the harness's own root when it exports one; these
    # tests want the walk-up path exercised instead.
    env.pop("CLAUDE_PROJECT_DIR", None)
    env.pop("CLUD_BAD_CMD_OVERRIDE", None)
    argv = [str(_cmd_scan_binary()), *(argv_extra or [])]
    return process.run(
        argv,
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )


def test_a_declared_pretooluse_hook_can_block_the_call(tmp_path: Path) -> None:
    repo = _make_repo(
        tmp_path,
        {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "command": _python_hook(
                        tmp_path / "block.py",
                        "import sys\nprint('run bash lint first', file=sys.stderr)\nsys.exit(2)\n",
                    ),
                }
            ]
        },
    )

    result = _run(
        tmp_path,
        payload={"tool_name": "Bash", "cwd": str(repo), "tool_input": {"command": "ls"}},
    )

    assert result.returncode == 2, result.stdout + result.stderr
    assert "run bash lint first" in result.stdout + result.stderr


def test_a_declared_hook_runs_rooted_at_the_repo_despite_a_drifted_cwd(
    tmp_path: Path,
) -> None:
    # The whole point of the dispatcher: the hook resolves its own relative
    # paths correctly even though the session wandered into a subdirectory.
    observed = tmp_path / "observed.txt"
    repo = _make_repo(
        tmp_path,
        {
            "PreToolUse": [
                {
                    "command": _python_hook(
                        tmp_path / "observe.py",
                        "import os\n"
                        f'open(r"{observed.as_posix()}", "w").write('
                        'os.getcwd() + "|" + os.environ.get("CLUD_PROJECT_DIR", ""))\n',
                    )
                }
            ]
        },
    )

    result = _run(
        tmp_path,
        payload={
            "tool_name": "Bash",
            # Drifted: the payload says we are in a subdirectory.
            "cwd": str(repo / "sub"),
            "tool_input": {"command": "ls"},
        },
    )

    assert result.returncode == 0, result.stdout + result.stderr
    cwd, project_dir = observed.read_text(encoding="utf-8").split("|")
    assert Path(cwd).resolve() == repo.resolve(), cwd
    assert Path(project_dir).resolve() == repo.resolve(), project_dir


def test_the_payload_is_forwarded_to_the_hook_unchanged(tmp_path: Path) -> None:
    seen = tmp_path / "seen.json"
    repo = _make_repo(
        tmp_path,
        {
            "PreToolUse": [
                {
                    "command": _python_hook(
                        tmp_path / "echo.py",
                        "import sys\n"
                        f'open(r"{seen.as_posix()}", "w").write(sys.stdin.read())\n',
                    )
                }
            ]
        },
    )

    payload = {"tool_name": "Bash", "cwd": str(repo), "tool_input": {"command": "ls -la"}}
    result = _run(tmp_path, payload=payload)

    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(seen.read_text(encoding="utf-8")) == payload


def test_a_matcher_scopes_the_hook_to_its_tool(tmp_path: Path) -> None:
    repo = _make_repo(
        tmp_path,
        {
            "PreToolUse": [
                {
                    "matcher": "Edit",
                    "command": _python_hook(
                        tmp_path / "edit_only.py", "import sys\nsys.exit(2)\n"
                    ),
                }
            ]
        },
    )

    bash = _run(
        tmp_path,
        payload={"tool_name": "Bash", "cwd": str(repo), "tool_input": {"command": "ls"}},
    )
    assert bash.returncode == 0, "a Bash call must not trip an Edit-scoped hook"

    edit = _run(
        tmp_path,
        payload={
            "tool_name": "Edit",
            "cwd": str(repo),
            "tool_input": {"file_path": str(repo / "a.txt")},
        },
    )
    assert edit.returncode == 2, edit.stdout + edit.stderr


def test_non_pretooluse_events_dispatch_only_under_their_event_flag(
    tmp_path: Path,
) -> None:
    marker = tmp_path / "stopped.txt"
    repo = _make_repo(
        tmp_path,
        {
            "Stop": [
                {
                    "command": _python_hook(
                        tmp_path / "on_stop.py",
                        f'open(r"{marker.as_posix()}", "w").write("ran")\n',
                    )
                }
            ]
        },
    )

    # A bare invocation still means PreToolUse, so the Stop hook stays put.
    bare = _run(
        tmp_path,
        payload={"tool_name": "Bash", "cwd": str(repo), "tool_input": {"command": "ls"}},
    )
    assert bare.returncode == 0, bare.stdout + bare.stderr
    assert not marker.exists(), "a bare invocation must not fire Stop hooks"

    stop = _run(
        tmp_path,
        payload={"cwd": str(repo)},
        argv_extra=["--event", "Stop"],
    )
    assert stop.returncode == 0, stop.stdout + stop.stderr
    assert marker.read_text(encoding="utf-8") == "ran"


def test_a_repo_that_declares_nothing_is_unaffected(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)

    result = _run(
        tmp_path,
        payload={"tool_name": "Bash", "cwd": str(repo), "tool_input": {"command": "ls"}},
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_broken_hook_fails_open_rather_than_wedging_the_session(
    tmp_path: Path,
) -> None:
    # Only exit 2 blocks. A guard that is merely broken must not become a wall
    # in front of every tool call -- that is the wedge this feature prevents.
    repo = _make_repo(
        tmp_path,
        {
            "PreToolUse": [
                {
                    "command": _python_hook(
                        tmp_path / "broken.py",
                        "import sys\nprint('boom', file=sys.stderr)\nsys.exit(1)\n",
                    )
                }
            ]
        },
    )

    result = _run(
        tmp_path,
        payload={"tool_name": "Bash", "cwd": str(repo), "tool_input": {"command": "ls"}},
    )

    assert result.returncode == 0, result.stdout + result.stderr
