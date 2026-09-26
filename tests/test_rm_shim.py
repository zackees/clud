"""Process coverage for the rm shim, the rm-file / rm-dir aliases and the hook.

Local rm requests outside the session's roots use guaranteed dry-run and never
a shell; inside `CLUD_RM_ROOTS` the child shim deletes in process (#1340).
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

import pytest

from tests import process


def binary(name: str) -> Path:
    suffix = ".exe" if sys.platform == "win32" else ""
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name + suffix)
        if clud
        else Path(__file__).resolve().parents[1] / "target/debug" / (name + suffix)
    )
    assert candidate.is_file(), f"build {name} before running process tests"
    return candidate


@pytest.fixture
def shim(tmp_path: Path) -> Path:
    target = tmp_path / ("rm.exe" if sys.platform == "win32" else "rm")
    shutil.copy2(binary("clud-shim"), target)
    return target


@pytest.mark.parametrize(
    "args",
    [
        ["-rf", "/"],
        ["-rf", ""],
        ["-rf", "../escape"],
        ["--no-preserve-root", "/"],
        ["-rf", "safe", "/"],
    ],
)
def test_rm_alias_denies_in_dry_run_without_daemon(shim: Path, args: list[str]) -> None:
    env = os.environ.copy()
    env.pop("CLUD_DAEMON_SOCKET", None)
    env.pop("CLUD_RM_ROOTS", None)
    env["CLUD_RM_DRY_RUN"] = "1"
    result = process.run([str(shim), *args], env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 2, result
    assert json.loads(result.stdout)["decision"] == "deny"


@pytest.mark.skipif(
    sys.platform != "linux", reason="mount validation is Linux-only; other platforms deny"
)
def test_rm_alias_safe_dry_run_preserves_file(shim: Path, tmp_path: Path) -> None:
    target = tmp_path / "disposable"
    target.write_text("must survive")
    env = os.environ.copy() | {"CLUD_RM_DRY_RUN": "1", "TEST_CI": ""}
    env.pop("CLUD_RM_ROOTS", None)
    result = process.run(
        [str(shim), "-rf", "--", str(target)], env=env, capture_output=True, text=True, timeout=30
    )
    assert result.returncode == 0, result
    assert json.loads(result.stdout)["dry_run"] is True
    assert target.read_text() == "must survive"


@pytest.mark.skipif(sys.platform != "linux", reason="real execution gate is Linux-only")
@pytest.mark.parametrize("exists", [False, True])
def test_real_rm_still_denies_without_ci_for_existing_or_missing_operand(
    shim: Path, tmp_path: Path, exists: bool
) -> None:
    env = {key: value for key, value in os.environ.items() if "CI" not in key}
    env.pop("CLUD_RM_DRY_RUN", None)
    env.pop("CLUD_RM_ROOTS", None)
    target = tmp_path / "never-created"
    if exists:
        target.write_text("must survive")
    result = process.run(
        [str(shim), "-rf", str(target)],
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 2, result
    assert json.loads(result.stdout)["decision"] == "deny"
    assert target.exists() == exists
    if exists:
        assert target.read_text() == "must survive"


@pytest.mark.parametrize("state", ["trusted", "replaced", "missing", "system", "unreadable"])
def test_hook_rm_identity(shim: Path, tmp_path: Path, state: str) -> None:
    directory = tmp_path / state
    directory.mkdir()
    target = directory / shim.name
    if state == "trusted":
        shutil.copy2(shim, target)
    elif state == "replaced":
        target.write_text("replaced")
        target.chmod(0o755)
    elif state == "system":
        system = next(
            (
                Path(p)
                for p in ("/bin/rm", "/usr/bin/rm", "/run/current-system/sw/bin/rm")
                if Path(p).is_file()
            ),
            None,
        )
        if system is None:
            pytest.skip("no Unix system rm")
        shutil.copyfile(system, target)
        target.chmod(0o755)
    elif state == "unreadable":
        if sys.platform == "win32":
            pytest.skip("POSIX executable permissions")
        shutil.copy2(shim, target)
        target.chmod(0)
    env = os.environ.copy() | {"PATH": str(directory)}
    # #1340: any shell command is checked; an agent's own `rm` is redirected
    # to rm-file / rm-dir before the identity check, so use a plain one.
    payload = json.dumps(
        {"tool_name": "Bash", "cwd": str(tmp_path), "tool_input": {"command": "ls ./build"}}
    )
    result = process.run(
        [str(binary("clud-block-bad-cmd"))],
        env=env,
        input=payload,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == (0 if state == "trusted" else 2), result
    if state != "trusted":
        assert json.loads(result.stdout)["hookSpecificOutput"]["permissionDecision"] == "deny"
        assert "rm identity" in result.stdout


@pytest.mark.parametrize(
    ("command", "marker"),
    [
        # An agent's own rm is redirected to rm-file / rm-dir (#1340) ...
        ("/bin/rm ./build", "rm-file ./build"),
        ("PATH=/bin; rm ./build", "rm-file ./build"),
        ("command -p rm ./build", "rm-file ./build"),
        ("bash -c 'rm ./build'", "rm-file ./build"),
        # ... and what does not type rm still has the identity check.
        ("busybox rm ./build", "rm identity"),
        ("hash -p /bin/rm rm", "rm identity"),
        ("PATH=/bin ./test", "rm identity"),
    ],
)
def test_hook_refuses_resolution_bypasses(
    shim: Path, tmp_path: Path, command: str, marker: str
) -> None:
    payload = json.dumps(
        {"tool_name": "Bash", "cwd": str(tmp_path), "tool_input": {"command": command}}
    )
    env = os.environ.copy() | {"PATH": str(shim.parent)}
    result = process.run(
        [str(binary("clud-cmd-scan"))],
        env=env,
        input=payload,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 2, result
    assert marker in result.stdout


def _alias(tmp_path: Path, name: str) -> Path:
    target = tmp_path / "bin" / (name + (".exe" if sys.platform == "win32" else ""))
    target.parent.mkdir(exist_ok=True)
    shutil.copy2(binary("clud-shim"), target)
    return target


def _session_env(tmp_path: Path, roots: list[Path]) -> dict[str, str]:
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = {key: value for key, value in os.environ.items() if "CI" not in key}
    env.pop("CLUD_RM_DRY_RUN", None)
    env |= {
        "HOME": str(home),
        "USERPROFILE": str(home),
        "CLUD_RM_ROOTS": os.pathsep.join(str(r) for r in roots),
    }
    return env


def test_child_rm_inside_the_roots_deletes_without_ci_or_docker(tmp_path: Path) -> None:
    """#1340: `./test` cleaning up after itself works on a developer machine."""
    repo = tmp_path / "repo"
    (repo / "build" / "obj").mkdir(parents=True)
    (repo / "build" / "obj" / "a.o").write_text("o")
    (repo / "stale.log").write_text("log")
    outside = tmp_path / "outside.txt"
    outside.write_text("keep")
    rm = _alias(tmp_path, "rm")
    env = _session_env(tmp_path, [repo])

    ok = process.run(
        [str(rm), "-rf", "build", "stale.log", "never-existed"],
        cwd=str(repo),
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert ok.returncode == 0, ok
    assert not (repo / "build").exists()
    assert not (repo / "stale.log").exists()

    missing = process.run(
        [str(rm), "never-existed"], cwd=str(repo), env=env, capture_output=True, text=True
    )
    assert missing.returncode == 1, missing
    assert "No such file" in missing.stderr

    for operand in (str(outside), "/", str(repo)):
        refused = process.run(
            [str(rm), "-rf", operand], cwd=str(repo), env=env, capture_output=True, text=True
        )
        assert refused.returncode == 2, (operand, refused)
        assert json.loads(refused.stdout)["decision"] == "deny"
    assert outside.read_text() == "keep"
    audit = list((tmp_path / "home" / ".clud" / "state" / "logs" / "rm").glob("*.jsonl"))
    records = [json.loads(line) for f in audit for line in f.read_text().splitlines()]
    assert any(r["role"] == "child" and r["exit"] == 0 for r in records), records


def test_rm_file_and_rm_dir_aliases_trash_inside_the_roots(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    (repo / "build").mkdir(parents=True)
    (repo / "notes.txt").write_text("n")
    env = _session_env(tmp_path, [repo])
    for name, operand in (("rm-dir", "build"), ("rm-file", "notes.txt")):
        result = process.run(
            [str(_alias(tmp_path, name)), operand],
            cwd=str(repo),
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 0, result
        assert result.stdout.startswith("trashed"), result.stdout
        assert not (repo / operand).exists()
    trash = tmp_path / "home" / ".clud" / "trash"
    manifests = sorted(trash.glob("*/.clud-rm.json"))
    assert len(manifests) == 2, list(trash.iterdir())
    refused = process.run(
        [str(_alias(tmp_path, "rm-file")), str(tmp_path / "home")],
        cwd=str(repo),
        env=env,
        capture_output=True,
        text=True,
    )
    assert refused.returncode == 1, refused
    assert "refused" in refused.stderr
