"""ci/act_ci.sh keeps act's output on the host, live and durable (#1548)."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "ci/act_ci.sh").read_text(encoding="utf-8")


def test_log_path_is_printed_before_act_starts() -> None:
    assert SCRIPT.index('echo "act_ci: log   $LOG"') < SCRIPT.index('run_act "$@" >>"$LOG"')


def test_act_status_survives_the_tee_and_greps_read_the_log() -> None:
    assert 'run_act "$@" >>"$LOG" 2>&1 || status=$?' in SCRIPT
    assert '[ "$status" -eq 0 ] || exit "$status"' in SCRIPT
    assert "grep -F 'PUBLIC_EVIDENCE ' \"$LOG\"" in SCRIPT


def test_log_dir_is_mounted_tracked_empty_and_user_only() -> None:
    assert '[stack.clud_act.mounts.act-logs]\nsource = ".clud/act-logs"' in (
        ROOT / "bosn.toml"
    ).read_text(encoding="utf-8")
    ignore = (ROOT / ".clud/act-logs/.gitignore").read_text(encoding="utf-8")
    assert ignore.split() == ["*", "!.gitignore"]
    assert "chmod 700" in SCRIPT and "umask 077" in SCRIPT
    assert "--exclude=./.clud/act-logs" in SCRIPT
