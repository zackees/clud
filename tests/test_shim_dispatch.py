"""Every clud alias fails open outside a valid session (#1546).

The shim list comes from `clud-shim --registry`, so a new registry row is
covered here without editing this file. Fake "real" binaries are the test
interpreter running a recorder script; PATH holds only the shim directory and
the fake's directory, so no test here can reach a system binary by accident.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

import pytest

from tests import process
from tests.shim_env import registry, session_env, session_key_names

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="POSIX recorder scripts")


def _binary(name: str) -> Path:
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name)
        if clud
        else Path(__file__).resolve().parents[1] / "target" / "debug" / name
    )
    assert candidate.is_file(), candidate
    return candidate


if sys.platform == "win32":
    SHIM = Path()
    REGISTRY: dict = {"shims": []}
else:
    SHIM = _binary("clud-shim")
    REGISTRY = registry(SHIM)
PASSTHROUGH = [spec["name"] for spec in REGISTRY["shims"] if spec["fallback"] == "Passthrough"]
NATIVE = [spec["name"] for spec in REGISTRY["shims"] if spec["fallback"] == "Native"]


def _recorder(path: Path, exit_code: int = 37) -> Path:
    """A fake real binary: logs argv and stdin, echoes to stdout, exits."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        "data = sys.stdin.read()\n"
        "with open(os.environ['REAL_LOG'], 'w') as f:\n"
        "    json.dump({'argv0': sys.argv[0], 'argv': sys.argv[1:], 'stdin': data}, f)\n"
        "sys.stdout.write('real:' + data)\n"
        f"sys.exit({exit_code})\n",
        encoding="utf-8",
    )
    path.chmod(0o755)
    return path


def _base_env(tmp_path: Path, *dirs: Path) -> dict[str, str]:
    """Inherited env minus every session key: an empty clud session."""
    dropped = session_key_names(SHIM) | {"CLUD_RM_ROOTS"}
    env = {k: v for k, v in os.environ.items() if k not in dropped}
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env.update(
        HOME=str(home),
        PATH=os.pathsep.join(str(d) for d in dirs),
        REAL_LOG=str(tmp_path / "real.json"),
    )
    return env


def _world(tmp_path: Path, name: str) -> tuple[Path, Path, Path]:
    shim_dir = tmp_path / "shim"
    shim_dir.mkdir(exist_ok=True)
    shim = shim_dir / name
    shutil.copy2(SHIM, shim)
    real = _recorder(tmp_path / "real" / name)
    return shim, real, tmp_path / "real.json"


def _run(argv: list[str], env: dict[str, str], cwd: Path, stdin: str = ""):
    return process.run(
        argv, env=env, cwd=str(cwd), input=stdin, capture_output=True, text=True, timeout=30
    )


def test_registry_lists_every_alias_the_session_installs() -> None:
    names = {spec["name"] for spec in REGISTRY["shims"]}
    assert {"python", "python3", "gh", "rm", "safe-rm"} <= names  # python-name-lint: allow
    assert "gh" in PASSTHROUGH
    assert "rm" in PASSTHROUGH
    assert "safe-rm" in NATIVE


@pytest.mark.parametrize("name", PASSTHROUGH)
def test_empty_session_passes_through_with_argv_stdio_and_exit(tmp_path: Path, name: str) -> None:
    shim, real, log = _world(tmp_path, name)
    env = _base_env(tmp_path, shim.parent, real.parent)
    args = ["-f", "a b", str(tmp_path / "operand")]
    result = _run([str(shim), *args], env, tmp_path, stdin="piped input")
    assert result.returncode == 37, result
    assert result.stdout == "real:piped input"
    assert len(result.stderr.splitlines()) <= 1, result.stderr
    record = json.loads(log.read_text(encoding="utf-8"))
    assert record["argv"] == args, "argv reaches the real binary unmodified"
    assert record["stdin"] == "piped input"
    assert Path(record["argv0"]).name == name


def _stale_sessions(tmp_path: Path, shim: Path) -> dict[str, dict[str, str]]:
    valid = session_env(SHIM, shim.parent)
    abi_key = REGISTRY["abi_key"]
    copy = tmp_path / "copy" / shim.name
    copy.parent.mkdir(exist_ok=True)
    shutil.copy2(SHIM, copy)
    stale: dict[str, dict[str, str]] = {
        "older clud (no stamp)": {k: v for k, v in valid.items() if k != abi_key},
        "other ABI": {**valid, abi_key: "0"},
    }
    targets = {
        "target deleted": str(tmp_path / "deleted"),
        "target in shim dir": str(shim),
        "target is a shim copy": str(copy),
    }
    for spec in REGISTRY["shims"]:
        if spec["name"] != shim.name:
            continue
        for key in spec["session_keys"]:
            if key.endswith("_TARGET"):
                for label, target in targets.items():
                    stale[label] = {**valid, key: target}
    if shim.name == "rm":
        stale["alias dir gone"] = {**valid, REGISTRY["session_dir_key"]: str(tmp_path / "gone")}
    return stale


@pytest.mark.parametrize("name", PASSTHROUGH)
def test_stale_session_passes_through(tmp_path: Path, name: str) -> None:
    shim, real, log = _world(tmp_path, name)
    assert real.is_file()
    for label, session in _stale_sessions(tmp_path, shim).items():
        if log.exists():
            log.unlink()
        env = _base_env(tmp_path, shim.parent, real.parent) | session
        result = _run([str(shim), "x"], env, tmp_path)
        assert result.returncode == 37, (label, result)
        assert json.loads(log.read_text(encoding="utf-8"))["argv"] == ["x"], label


@pytest.mark.parametrize("name", PASSTHROUGH)
def test_shim_alone_on_path_is_command_not_found_not_recursion(
    tmp_path: Path, name: str
) -> None:
    shim, _, _ = _world(tmp_path, name)
    second = tmp_path / "second"
    second.mkdir()
    shutil.copy2(SHIM, second / name)
    env = _base_env(tmp_path, shim.parent, second)
    result = _run([str(shim), "x"], env, tmp_path)
    assert result.returncode == 127, result
    assert len(result.stderr.splitlines()) == 1, result.stderr
    assert "command not found" in result.stderr


@pytest.mark.parametrize("name", NATIVE)
def test_native_shim_runs_its_native_mode_outside_a_session(tmp_path: Path, name: str) -> None:
    shim, _, log = _world(tmp_path, name)
    work = tmp_path / "work"
    work.mkdir()
    victim = work / "victim.txt"
    victim.write_text("keep", encoding="utf-8")
    env = _base_env(tmp_path, shim.parent)
    result = _run([str(shim), "--dry-run", str(victim)], env, work)
    assert result.returncode == 0, result
    assert victim.exists()
    assert not log.exists(), "a native shim never relays"


def test_version_skew_session_resolves_real_binaries_after_new_aliases_land(
    tmp_path: Path,
) -> None:
    """A 2.8.14-style session env meets aliases installed by a newer clud."""
    shim_dir = tmp_path / "shim"
    shim_dir.mkdir()
    real_dir = tmp_path / "real"
    for name in PASSTHROUGH:
        _recorder(real_dir / name)
    old_session = _base_env(tmp_path, shim_dir, real_dir) | {
        REGISTRY["session_dir_key"]: str(shim_dir)
    }
    for name in PASSTHROUGH:  # the newer clud installs its aliases
        shutil.copy2(SHIM, shim_dir / name)
    log = tmp_path / "real.json"
    for name in PASSTHROUGH:
        result = _run([str(shim_dir / name), "--version"], old_session, tmp_path)
        assert result.returncode == 37, (name, result)
        assert json.loads(log.read_text(encoding="utf-8"))["argv"] == ["--version"], name


def test_git_credential_helper_reaches_real_gh_without_askpass(tmp_path: Path) -> None:
    """#1546's repro: `!gh auth git-credential` with gh's session keys removed."""
    git = shutil.which("git")
    if git is None:
        pytest.skip("git is not installed")
    shim_dir = tmp_path / "shim"
    shim_dir.mkdir()
    shutil.copy2(SHIM, shim_dir / "gh")
    real_gh = tmp_path / "real" / "gh"
    real_gh.parent.mkdir()
    real_gh.write_text(
        f"#!{sys.executable}\n"
        "import sys\n"
        "if sys.argv[1:] == ['auth', 'git-credential', 'get']:\n"
        "    sys.stdin.read()\n"
        "    print('protocol=https\\nhost=github.com')\n"
        "    print('username=x-access-token\\npassword=fixture-token')\n"
        "    sys.exit(0)\n"
        "sys.exit(1)\n",
        encoding="utf-8",
    )
    real_gh.chmod(0o755)
    askpass_log = tmp_path / "askpass.log"
    askpass = tmp_path / "askpass"
    askpass.write_text(
        f"#!{sys.executable}\nopen({str(askpass_log)!r}, 'a').write('called\\n')\nprint('bad')\n",
        encoding="utf-8",
    )
    askpass.chmod(0o755)
    env = _base_env(tmp_path, shim_dir, real_gh.parent, Path(git).parent)
    # A session from before the gh alias existed: the dir is on PATH, no gh keys.
    env[REGISTRY["session_dir_key"]] = str(shim_dir)
    env.update(
        GIT_CONFIG_NOSYSTEM="1",
        GIT_CONFIG_GLOBAL=str(tmp_path / "gitconfig"),
        GIT_ASKPASS=str(askpass),
        SSH_ASKPASS=str(askpass),
        GIT_TERMINAL_PROMPT="0",
    )
    result = _run(
        [git, "-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential",
         "credential", "fill"],
        env,
        tmp_path,
        stdin="protocol=https\nhost=github.com\n\n",
    )
    assert result.returncode == 0, result
    assert "password=fixture-token" in result.stdout
    assert not askpass_log.exists(), "the credential helper answered; askpass never ran"
