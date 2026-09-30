"""ci/act_ci.sh keeps act's output on the host, live, durable and split (#1548, #1549)."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "ci/act_ci.sh").read_text(encoding="utf-8")


def test_log_paths_are_printed_before_act_starts() -> None:
    start = SCRIPT.index('run_act "$@" >>"$OUT" 2>>"$ERR"')
    for stream in ("stdout $OUT", "stderr $ERR", "status $STATUS"):
        assert SCRIPT.index(f'echo "act_ci: {stream}"') < start


def test_stdout_and_stderr_are_separate_files_and_act_status_survives() -> None:
    assert 'run_act "$@" >>"$OUT" 2>>"$ERR" || status=$?' in SCRIPT
    assert '[ "$status" -eq 0 ] || exit "$status"' in SCRIPT
    assert '>>"$OUT" 2>&1' not in SCRIPT
    assert "2>&1" not in SCRIPT[SCRIPT.index("tail -n +1 -f"):SCRIPT.index("sleep 1")]


def test_status_records_come_from_acts_own_step_lines_and_cancellation() -> None:
    assert '" Success - "' in SCRIPT
    assert '" Failure - "' in SCRIPT
    assert '"conclusion' in SCRIPT
    assert 'trap \'printf "{\\"event\\":\\"cancelled\\"}' in SCRIPT
    assert "step(s) failed; first:" in SCRIPT


def test_public_evidence_greps_still_read_the_output() -> None:
    assert 'cat "$OUT" "$ERR" >"$LOG"' in SCRIPT
    assert "grep -F 'PUBLIC_EVIDENCE ' \"$LOG\"" in SCRIPT


def test_log_dir_is_mounted_tracked_empty_and_user_only() -> None:
    assert '[stack.clud_act.mounts.act-logs]\nsource = ".clud/act-logs"' in (
        ROOT / "bosn.toml"
    ).read_text(encoding="utf-8")
    ignore = (ROOT / ".clud/act-logs/.gitignore").read_text(encoding="utf-8")
    assert ignore.split() == ["*", "!.gitignore"]
    assert "chmod 700" in SCRIPT
    assert "umask 077" in SCRIPT
    assert "--exclude=./.clud/act-logs" in SCRIPT
