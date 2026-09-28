"""Process regressions for the session-scoped gh alias (#1518)."""

from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

import pytest

from tests import process


def _binary(name: str) -> Path:
    suffix = ".exe" if sys.platform == "win32" else ""
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name + suffix)
        if clud
        else Path(__file__).resolve().parents[1] / "target" / "debug" / (name + suffix)
    )
    assert candidate.is_file(), candidate
    return candidate


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
def test_pr_checks_watch_routes_to_bundled_watcher_and_preserves_exit(tmp_path: Path) -> None:
    shim_dir = tmp_path / "shim"
    shim_dir.mkdir()
    shim = shim_dir / "gh"
    shutil.copy2(_binary("clud-shim"), shim)

    real_gh_log = tmp_path / "real-gh-args"
    real_gh = tmp_path / "real-gh"
    real_gh.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$@" > "$REAL_GH_LOG"\nexit 93\n',
        encoding="utf-8",
    )
    real_gh.chmod(0o755)
    clud_log = tmp_path / "clud-args"
    fake_clud = tmp_path / "fake-clud"
    fake_clud.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$@" > "$CLUD_LOG"\nexit 4\n',
        encoding="utf-8",
    )
    fake_clud.chmod(0o755)
    env = os.environ.copy()
    env.update(
        CLUD_GH_SHIM_TARGET=str(real_gh),
        CLUD_EXE=str(fake_clud),
        REAL_GH_LOG=str(real_gh_log),
        CLUD_LOG=str(clud_log),
        PATH=f"{shim_dir}{os.pathsep}{env['PATH']}",
    )

    result = process.run(
        [
            str(shim), "--repo", "zackees/clud", "pr", "checks", "123",
            "--watch", "--fail-fast", "-i", "7",
        ],
        env=env,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=15,
    )

    assert result.returncode == 4, result
    assert not real_gh_log.exists()
    assert clud_log.read_text(encoding="utf-8").splitlines() == [
        "tool", "run", "github/pr_merge_watch.py", "123",
        "--repo", "zackees/clud", "--interval", "7",
    ]


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
def test_non_watch_gh_relays_arguments_and_exit(tmp_path: Path) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    real_gh = tmp_path / "real-gh"
    real_gh.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\nexit 37\n', encoding="utf-8")
    real_gh.chmod(0o755)
    env = os.environ.copy()
    env["CLUD_GH_SHIM_TARGET"] = str(real_gh)
    result = process.run(
        [str(alias), "api", "repos/zackees/clud", "--method", "GET"],
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 37
    assert result.stdout.splitlines() == ["api", "repos/zackees/clud", "--method", "GET"]


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
@pytest.mark.parametrize(
    "selector", ["", "my-branch", "pr", "checks", "https://github.com/zackees/clud/pull/9"],
)
def test_watch_resolves_non_numeric_selector(tmp_path: Path, selector: str) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    lookup_log = tmp_path / "lookup-args"
    real_gh = tmp_path / "real-gh"
    real_gh.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$@" > "$LOOKUP_LOG"\nprintf "42\\n"\n',
        encoding="utf-8",
    )
    real_gh.chmod(0o755)
    watcher_log = tmp_path / "watcher-args"
    fake_clud = tmp_path / "fake-clud"
    fake_clud.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$@" > "$WATCHER_LOG"\nexit 5\n',
        encoding="utf-8",
    )
    fake_clud.chmod(0o755)
    env = os.environ.copy()
    env.update(
        CLUD_GH_SHIM_TARGET=str(real_gh), CLUD_EXE=str(fake_clud),
        LOOKUP_LOG=str(lookup_log), WATCHER_LOG=str(watcher_log),
    )
    args = [str(alias), "pr", "checks"]
    if selector:
        args.append(selector)
    args.extend(["--watch", "-R", "zackees/clud"])
    result = process.run(
        args, env=env, cwd=tmp_path, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 5, result
    expected_lookup = ["pr", "view"]
    if selector:
        expected_lookup.append(selector)
    expected_lookup += ["--json", "number", "--jq", ".number", "--repo", "zackees/clud"]
    assert lookup_log.read_text(encoding="utf-8").splitlines() == expected_lookup
    assert watcher_log.read_text(encoding="utf-8").splitlines() == [
        "tool", "run", "github/pr_merge_watch.py", "42", "--repo", "zackees/clud",
    ]


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
@pytest.mark.parametrize("flag", ["--json", "--jq", "--template", "--web", "--required"])
def test_incompatible_watch_flags_fail_closed(tmp_path: Path, flag: str) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    real_gh = tmp_path / "real-gh"
    real_gh.write_text("#!/bin/sh\nexit 93\n", encoding="utf-8")
    real_gh.chmod(0o755)
    env = os.environ.copy()
    env["CLUD_GH_SHIM_TARGET"] = str(real_gh)
    result = process.run(
        [str(alias), "pr", "checks", "123", "--watch", flag],
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 2, result
    assert "unsupported or ambiguous" in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
def test_missing_target_is_not_recursive_fallback(tmp_path: Path) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    env = os.environ.copy()
    env.pop("CLUD_GH_SHIM_TARGET", None)
    result = process.run(
        [str(alias), "pr", "checks", "123", "--watch"],
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 127
    assert "CLUD_GH_SHIM_TARGET" in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX executable modes")
def test_unexecutable_target_reports_exec_failure(tmp_path: Path) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    real_gh = tmp_path / "real-gh"
    real_gh.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    real_gh.chmod(0o644)
    env = os.environ.copy()
    env["CLUD_GH_SHIM_TARGET"] = str(real_gh)
    result = process.run(
        [str(alias), "pr", "view", "123"],
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 126, result
    assert "failed to exec" in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")
def test_pr_lookup_output_is_bounded(tmp_path: Path) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    real_gh = tmp_path / "real-gh"
    real_gh.write_text("#!/bin/sh\nprintf '%0200d\\n' 1\n", encoding="utf-8")
    real_gh.chmod(0o755)
    env = os.environ.copy()
    env["CLUD_GH_SHIM_TARGET"] = str(real_gh)
    result = process.run(
        [str(alias), "pr", "checks", "--watch"],
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 2, result
    assert "output was too large" in result.stderr


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX shell fixture")
def test_shim_reaches_real_bundled_watcher(tmp_path: Path) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    real_gh = tmp_path / "real-gh"
    real_gh.write_text("#!/bin/sh\nexit 93\n", encoding="utf-8")
    real_gh.chmod(0o755)
    watcher = (
        Path(__file__).resolve().parents[1]
        / "crates/clud-bin/assets/tools/github/pr_merge_watch.py"
    )
    fake_clud = tmp_path / "fake-clud"
    fake_clud.write_text(
        '#!/bin/sh\n[ "$1" = tool ] && [ "$2" = run ] || exit 98\n'
        f'shift 3\nexec "{sys.executable}" "{watcher}" "$@"\n',
        encoding="utf-8",
    )
    fake_clud.chmod(0o755)
    env = os.environ.copy()
    env.update(
        CLUD_GH_SHIM_TARGET=str(real_gh), CLUD_EXE=str(fake_clud),
        CLUD_PR_MERGE_WATCH_DRY_RUN="1",
    )
    result = process.run(
        [str(alias), "pr", "checks", "123", "--watch", "--repo", "zackees/clud"],
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == 0, result
    assert "DRY-RUN pr=123 repo=zackees/clud" in result.stdout


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX shell fixture")
@pytest.mark.parametrize(
    ("scenario", "exit_code", "should_cancel"),
    [("fail", 1, True), ("review", 2, True), ("no_checks", 8, False)],
)
def test_shim_preserves_real_watcher_outcomes(
    tmp_path: Path, scenario: str, exit_code: int, should_cancel: bool
) -> None:
    alias = tmp_path / "gh"
    shutil.copy2(_binary("clud-shim"), alias)
    real_gh = tmp_path / "real-gh"
    real_gh.write_text("#!/bin/sh\nexit 93\n", encoding="utf-8")
    real_gh.chmod(0o755)
    harness = Path(__file__).resolve().parent / "fixtures/gh_watch_harness.py"
    fake_clud = tmp_path / "fake-clud"
    fake_clud.write_text(
        '#!/bin/sh\n[ "$1" = tool ] && [ "$2" = run ] || exit 98\n'
        f'shift 3\nexec "{sys.executable}" "{harness}" "$@"\n',
        encoding="utf-8",
    )
    fake_clud.chmod(0o755)
    cancel_log = tmp_path / "cancel.txt"
    env = os.environ.copy()
    env.update(
        CLUD_GH_SHIM_TARGET=str(real_gh), CLUD_EXE=str(fake_clud),
        GH_WATCH_SCENARIO=scenario, GH_WATCH_CANCEL_LOG=str(cancel_log),
    )
    result = process.run(
        [str(alias), "pr", "checks", "123", "--watch", "--repo", "zackees/clud"],
        env=env, cwd=tmp_path, capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == exit_code, result
    assert cancel_log.exists() is should_cancel
    if should_cancel:
        assert cancel_log.read_text(encoding="utf-8") == "123 abc123\n"


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-native alias dispatch")
def test_windows_gh_alias_dispatches_watch_and_relay(tmp_path: Path) -> None:
    shim = tmp_path / "gh.exe"
    shutil.copy2(_binary("clud-shim"), shim)
    recorder = tmp_path / "real-gh.exe"
    shutil.copy2(Path(os.environ["CLUD_TEST_MOCK_AGENT_BINARY"]), recorder)
    recorded = tmp_path / "argv.json"
    env = os.environ.copy()
    env.update(CLUD_GH_SHIM_TARGET=str(recorder), MOCK_RM_STUB_LOG=str(recorded))
    plain = process.run(
        [str(shim), "pr", "view", "123"], env=env, cwd=tmp_path,
        capture_output=True, text=True, timeout=30,
    )
    assert plain.returncode == 0, plain
    assert json.loads(recorded.read_text(encoding="utf-8")) == ["pr", "view", "123"]
    env["CLUD_EXE"] = str(recorder)
    watched = process.run(
        [str(shim), "pr", "checks", "123", "--watch"], env=env, cwd=tmp_path,
        capture_output=True, text=True, timeout=30,
    )
    assert watched.returncode == 0, watched
    assert json.loads(recorded.read_text(encoding="utf-8")) == [
        "tool", "run", "github/pr_merge_watch.py", "123",
    ]
