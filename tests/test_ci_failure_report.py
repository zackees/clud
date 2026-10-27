"""Failure diagnostics stay actionable without streaming full CI logs (#2228)."""

import importlib.util
from pathlib import Path

import pytest

TOOL = (
    Path(__file__).resolve().parents[1]
    / "crates/clud-bin/assets/tools/github/ci_failure_report.py"
)


@pytest.fixture
def report():
    spec = importlib.util.spec_from_file_location("ci_failure_report", TOOL)
    assert spec
    assert spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_rust_diagnostic_precedes_generic_summary(report):
    prefix = "Release gate / Rust\tLint\t2026-10-26T16:27:29.9639051Z "
    raw = "".join(prefix + line + "\n" for line in [
        "checking dependency" for _ in range(100)
    ] + [
        "\x1b[1merror\x1b[0m: casting to the same type is unnecessary",
        "  --> rust/obs-data/src/lib.rs:42:9",
        "   |",
        "42 |     ptr as *const u8",
        "   |     ^^^^^^^^^^^^^^^ help: remove the cast",
        "error: could not compile `obs-data` (lib) due to 1 previous error",
    ])
    summary = report.summarize(raw)
    assert "casting to the same type is unnecessary" in summary
    assert "rust/obs-data/src/lib.rs:42:9" in summary
    assert "Release gate / Rust" not in summary
    assert "\x1b" not in summary
    assert len(summary.encode()) < len(raw.encode()) // 2


def test_caught_panic_is_not_reported_as_failure(report):
    raw = "\n".join([
        "thread 'tests::expected' panicked at src/lib.rs:3: caught",
        "test tests::expected ... ok",
        "FAILED tests/test_api.py::test_bad - AssertionError: incorrect",
    ])
    summary = report.summarize(raw)
    assert "AssertionError: incorrect" in summary
    assert "panicked" not in summary


def test_bounds_and_explicit_truncation(report):
    summary = report.summarize("\n".join("error: " + "ü" * 10000 for _ in range(100)))
    assert len(summary.encode()) <= report.MAX_OUTPUT_BYTES
    assert len(summary.splitlines()) <= report.MAX_OUTPUT_LINES
    assert "truncated" in summary.lower()


def test_annotation_and_unknown_are_explicit(report):
    assert "linker failed" in report.summarize("##[error]linker failed")
    assert "no actionable" in report.summarize("some unexpected output").lower()


def test_job_endpoint_works_while_sibling_jobs_run(report, monkeypatch, tmp_path, capsys):
    log = "2026-10-26T16:27:29.9639051Z error[E0308]: mismatched types\n" \
          "2026-10-26T16:27:29.9639051Z  --> src/lib.rs:9:5\n"
    calls = []

    def fetch(path):
        calls.append(path)
        if path.endswith("/logs"):
            return log.encode()
        return b'{"run_id": 123, "status": "completed", "conclusion": "failure"}'

    monkeypatch.setattr(report, "github_get", fetch)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    assert report.main(["--repo", "owner/repo", "--run", "123", "--job", "456"]) == 0
    output = capsys.readouterr().out
    assert "mismatched types" in output
    assert "src/lib.rs:9:5" in output
    assert any(path.endswith("actions/jobs/456/logs") for path in calls)
    assert not any("runs/123/logs" in path for path in calls)
    assert any(p.read_bytes() == log.encode() for p in tmp_path.iterdir())


def test_missing_or_empty_log_is_not_success(report, monkeypatch, tmp_path, capsys):
    def fetch(path):
        if path.endswith("/logs"):
            return b""
        return b'{"run_id": 123, "status": "completed", "conclusion": "failure"}'

    monkeypatch.setattr(report, "github_get", fetch)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    assert report.main(["--repo", "owner/repo", "--run", "123", "--job", "456"]) != 0
    assert "empty" in capsys.readouterr().err.lower()


def test_incomplete_job_and_wrong_run_are_rejected(report, monkeypatch, tmp_path, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    for metadata, reason in [
        (b'{"run_id": 123, "status": "in_progress", "conclusion": null}', "in_progress"),
        (b'{"run_id": 999, "status": "completed", "conclusion": "failure"}', "run"),
    ]:
        monkeypatch.setattr(report, "github_get", lambda path, metadata=metadata: metadata)
        assert report.main(["--repo", "owner/repo", "--run", "123", "--job", "456"]) != 0
        assert reason in capsys.readouterr().err.lower()


def test_artifact_failure_is_not_silent(report, monkeypatch, tmp_path, capsys):
    blocker = tmp_path / "file"
    blocker.write_text("not a directory")
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(blocker / "nested"))
    monkeypatch.setattr(
        report,
        "github_get",
        lambda path: (b'error: failed\n' if path.endswith('/logs') else
                      b'{"run_id": 123, "status": "completed", "conclusion": "failure"}'),
    )
    assert report.main(["--repo", "owner/repo", "--run", "123", "--job", "456"]) != 0
    assert "artifact" in capsys.readouterr().err.lower()


def test_saved_log_can_be_inspected_without_github(report, monkeypatch, tmp_path, capsys):
    artifact = tmp_path / "full.log"
    artifact.write_text("one\ntwo\nthree\nfour\n", encoding="utf-8")

    def unexpected_fetch(_path):
        raise AssertionError("continuation must not fetch logs again")

    monkeypatch.setattr(report, "github_get", unexpected_fetch)
    assert report.main(["--artifact", str(artifact), "--start-line", "2", "--line-count", "2"]) == 0
    output = capsys.readouterr().out
    assert "two" in output
    assert "three" in output
    assert "four" not in output


def test_multiple_failures_keep_evidence(report):
    raw = "\n".join([
        "FAILED tests/test_one.py::test_first - AssertionError: first",
        "FAILED tests/test_two.py::test_second - ValueError: second",
        "##[error]Process completed with exit code 1.",
    ])
    summary = report.summarize(raw)
    assert "test_first" in summary
    assert "test_second" in summary


def test_malformed_unicode_is_safe_and_bounded(report):
    raw = b"error: invalid \xff bytes\n --> src/lib.rs:1:1\n".decode("utf-8", errors="replace")
    summary = report.summarize(raw)
    assert "invalid" in summary
    assert "src/lib.rs:1:1" in summary
    assert len(summary.encode("utf-8")) <= report.MAX_OUTPUT_BYTES


def test_unavailable_log_fails_without_claiming_success(report, monkeypatch, tmp_path, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))

    def fetch(path):
        if path.endswith("/logs"):
            raise RuntimeError("GitHub HTTP 502; log may still be uploading")
        return b'{"run_id": 123, "status": "completed", "conclusion": "failure"}'

    monkeypatch.setattr(report, "github_get", fetch)
    assert report.main(["--repo", "owner/repo", "--run", "123", "--job", "456"]) != 0
    assert "502" in capsys.readouterr().err


def test_lander_guidance_prefers_bundled_diagnostics():
    root = Path(__file__).resolve().parents[1]
    guidance = (root / "crates/clud-bin/assets/skills/grind-land/SKILL.md").read_text()
    assert "github/ci_failure_report.py" in guidance
    assert "--repo" in guidance
    assert "--run" in guidance
    assert "--job" in guidance
