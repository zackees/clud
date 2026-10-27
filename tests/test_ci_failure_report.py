"""Failure diagnostics stay actionable without streaming full CI logs (#2228)."""

import importlib.util
import os
import shlex
import sys
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



def mock_download(monkeypatch, report, fetch):
    def download(path, destination, _host):
        destination.write_bytes(fetch(path))

    monkeypatch.setattr(report, "github_download", download)


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
    mock_download(monkeypatch, report, fetch)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    assert report.main(["--repo", "owner/repo", "--run", "123", "--job", "456"]) == 0
    output = capsys.readouterr().out
    assert "mismatched types" in output
    assert "src/lib.rs:9:5" in output
    assert any(path.endswith("actions/jobs/456/logs") for path in calls)
    assert not any("runs/123/logs" in path for path in calls)
    assert any(p.read_bytes() == log.encode() for p in (tmp_path / "ci-failure-private").iterdir())


def test_missing_or_empty_log_is_not_success(report, monkeypatch, tmp_path, capsys):
    def fetch(path):
        if path.endswith("/logs"):
            return b""
        return b'{"run_id": 123, "status": "completed", "conclusion": "failure"}'

    monkeypatch.setattr(report, "github_get", fetch)
    mock_download(monkeypatch, report, fetch)
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
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    artifact = report.save_artifact(b"one\ntwo\nthree\nfour\n")

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
    mock_download(monkeypatch, report, fetch)
    assert report.main(["--repo", "owner/repo", "--run", "123", "--job", "456"]) != 0
    assert "502" in capsys.readouterr().err


def test_lander_guidance_prefers_bundled_diagnostics():
    root = Path(__file__).resolve().parents[1]
    guidance = (root / "crates/clud-bin/assets/skills/grind-land/SKILL.md").read_text()
    assert "github/ci_failure_report.py" in guidance
    assert "--repo" in guidance
    assert "--run" in guidance
    assert "--job" in guidance


def test_broker_stale_response_is_not_accepted(report, monkeypatch):
    from types import SimpleNamespace

    monkeypatch.setattr(
        report.RunningProcess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=b"cached result",
            stderr=b"clud: gh read broker: rate-limit reserve",
        ),
    )
    with pytest.raises(RuntimeError, match="stale"):
        report.github_get("repos/o/r/actions/jobs/1/logs")


def test_saved_artifacts_are_unique_and_private(report, tmp_path, monkeypatch):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    first = report.save_artifact(b"first")
    second = report.save_artifact(b"second")
    assert first != second
    assert first.read_bytes() == b"first"
    assert second.read_bytes() == b"second"
    if sys.platform != "win32":
        assert first.stat().st_mode & 0o077 == 0


def test_byte_continuation_reaches_end_of_long_line(report, tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    artifact = report.save_artifact(("a" * 7000 + "END").encode())
    assert report.main(["--artifact", str(artifact), "--byte-offset", "6980"]) == 0
    assert "END" in capsys.readouterr().out


def test_github_request_captures_binary_output(report, monkeypatch):
    from types import SimpleNamespace

    calls = []

    def fake_run(*args, **kwargs):
        calls.append((args, kwargs))
        return SimpleNamespace(returncode=0, stdout=b"raw\xff", stderr=b"")

    monkeypatch.setattr(report.RunningProcess, "run", fake_run)
    assert report.github_get("repos/o/r/actions/jobs/1/logs") == b"raw\xff"
    assert calls[0][0][0] == [
        "gh", "api", "repos/o/r/actions/jobs/1/logs", "--allow-escape-sequences"
    ]
    assert calls[0][1]["text"] is False
    assert calls[0][1]["timeout"] == 60


def test_line_continuation_does_not_skip_byte_capped_lines(report, tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    raw = "".join(f"line-{i:02d} " + "x" * 480 + "\n" for i in range(40))
    artifact = report.save_artifact(raw.encode())
    assert report.main(["--artifact", str(artifact), "--line-count", "40"]) == 0
    output = capsys.readouterr().out
    shown = [int(line.split()[0]) for line in output.splitlines() if "[byte " in line]
    assert 0 < len(shown) < 40
    assert f"Continue at line {shown[-1] + 1}:" in output
    assert report.main([
        "--artifact", str(artifact), "--start-line", str(shown[-1] + 1), "--line-count", "40"
    ]) == 0
    assert f"line-{shown[-1]:02d}" in capsys.readouterr().out


def test_continuation_rejects_outside_files_and_symlinks(report, tmp_path, monkeypatch, capsys):
    root = tmp_path / "output"
    root.mkdir(mode=0o700)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(root))
    secret = tmp_path / "secret"
    secret.write_text("sensitive contents")
    assert report.main(["--artifact", str(secret)]) != 0
    assert "sensitive contents" not in capsys.readouterr().out
    private = report.artifact_root()
    link = private / "ci-failure-12345678.log"
    link.symlink_to(secret)
    assert report.main(["--artifact", str(link)]) != 0
    assert "sensitive contents" not in capsys.readouterr().out


def test_control_characters_in_artifact_path_are_rejected(report, tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    malicious = tmp_path / "ci-failure-1234\n678.log"
    assert report.main(["--artifact", str(malicious)]) != 0
    err = capsys.readouterr().err
    assert "control" in err
    assert len(err.splitlines()) == 1


def test_nextest_caught_panic_is_not_the_cause(report):
    raw = "\n".join([
        "thread 'tests::expected' panicked at src/lib.rs:3: caught",
        "PASS [  0.001s] my_crate tests::expected",
        "FAILED tests/test_api.py::test_bad - AssertionError: incorrect",
    ])
    summary = report.summarize(raw)
    assert "panicked" not in summary
    assert "AssertionError: incorrect" in summary


def test_enterprise_host_reaches_metadata_and_log(report, monkeypatch, tmp_path, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    calls = []

    def get(path, host):
        calls.append((path, host))
        return b'{"run_id": 123, "status": "completed", "conclusion": "failure"}'

    def download(path, destination, host):
        calls.append((path, host))
        destination.write_bytes(b"error: compile failed\n")

    monkeypatch.setattr(report, "github_get", get)
    monkeypatch.setattr(report, "github_download", download)
    assert report.main(["--repo", "ghe.example.com:8443/o/r", "--run", "123", "--job", "456"]) == 0
    assert all(host == "ghe.example.com:8443" for _, host in calls)
    assert len(calls) == 2
    assert "compile failed" in capsys.readouterr().out


def test_large_log_stream_retains_late_failure(report, tmp_path, monkeypatch):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    raw = b"ok\n" * 500000 + b"error: late failure\n --> src/lib.rs:2:3\n"
    artifact = report.save_artifact(raw)
    with report.open_artifact(artifact) as stream:
        result = report.summarize_stream(stream)
    assert "late failure" in result
    assert "src/lib.rs:2:3" in result
    assert len(result.encode()) <= report.MAX_OUTPUT_BYTES


def test_byte_continuation_escapes_c1_control(report, tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    artifact = report.save_artifact("\u009b31m".encode())
    assert report.main(["--artifact", str(artifact), "--byte-offset", "0"]) == 0
    output = capsys.readouterr().out
    assert "\u009b" not in output
    assert "\\u009b" in output


def test_nextest_duplicate_name_reports_attribution_uncertainty(report):
    raw = "\n".join([
        "thread 'tests::shared' panicked at src/lib.rs:3: caught",
        "PASS [ 0.1s] first tests::shared",
        "FAIL [ 0.2s] second tests::shared",
    ])
    summary = report.summarize(raw)
    assert "FAIL [ 0.2s] second tests::shared" in summary
    assert "attribution is ambiguous" in summary


def test_insecure_output_directory_is_rejected(report, tmp_path, monkeypatch):
    if sys.platform == "win32":
        pytest.skip("POSIX mode bits do not control Windows ACLs")
    root = tmp_path / "shared"
    root.mkdir(mode=0o755)
    private = root / "ci-failure-private"
    private.mkdir(mode=0o755)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(root))
    with pytest.raises(ValueError, match="private"):
        report.save_artifact(b"data")


def test_windows_path_quoting(report):
    path = Path("C:/Users/Name With Spaces/ci-failure-12345678.log")
    assert report.quote_path(path, windows=True) == f'"{path}"'


def test_default_gh_host_is_consistent_for_both_requests(report, tmp_path, monkeypatch):
    from types import SimpleNamespace

    calls = []

    def fake_run(command, **kwargs):
        calls.append(command)
        return SimpleNamespace(returncode=0, stdout=b"{}", stderr=b"")

    monkeypatch.setenv("GH_HOST", "ghe.example.com")
    monkeypatch.setattr(report.RunningProcess, "run", fake_run)
    report.github_get("repos/o/r/actions/jobs/1")
    report.github_download("repos/o/r/actions/jobs/1/logs", tmp_path / "report.log", None)
    assert "--hostname" not in calls[0]
    assert "--hostname" not in calls[1][2 if os.name != "nt" else 4]


@pytest.mark.skipif(os.name == "nt", reason="POSIX executable fixture")
def test_real_download_preserves_large_binary_output(report, tmp_path, monkeypatch):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    gh = bin_dir / "gh"
    code = "import sys; sys.stdout.buffer.write(b'x'*2000000+b'\\xffEND')"
    gh.write_text(f"#!/bin/sh\nexec {shlex.quote(sys.executable)} -c {shlex.quote(code)}\n")
    gh.chmod(0o700)
    monkeypatch.setenv("PATH", str(bin_dir) + os.pathsep + os.environ["PATH"])
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    artifact = report.save_artifact(b"")
    report.github_download("repos/o/r/actions/jobs/1/logs", artifact, None)
    assert artifact.stat().st_size == 2000004
    with report.open_artifact(artifact) as stream:
        stream.seek(-4, 2)
        assert stream.read() == b"\xffEND"


def test_existing_shared_output_root_gets_private_child(report, tmp_path, monkeypatch):
    root = tmp_path / "shared"
    root.mkdir(mode=0o755)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(root))
    artifact = report.save_artifact(b"data")
    assert artifact.parent.name == "ci-failure-private"
    assert artifact.read_bytes() == b"data"
    if hasattr(os, "getuid"):
        assert artifact.parent.stat().st_mode & 0o077 == 0


def test_windows_quoting_rejects_powershell_metacharacters(report):
    path = Path("C:/Users/$name/ci-failure-12345678.log")
    with pytest.raises(ValueError, match="Windows shell"):
        report.quote_path(path, windows=True)


def test_writable_output_parent_is_rejected(report, tmp_path, monkeypatch):
    if sys.platform == "win32":
        pytest.skip("POSIX mode bits do not control Windows ACLs")
    root = tmp_path / "shared"
    root.mkdir(mode=0o700)
    root.chmod(0o777)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(root))
    with pytest.raises(ValueError, match="writable"):
        report.save_artifact(b"data")


def test_untrusted_sticky_ancestor_is_rejected(report, monkeypatch):
    from types import SimpleNamespace

    monkeypatch.setattr(report.os, "getuid", lambda: 1000, raising=False)
    untrusted = SimpleNamespace(st_uid=1001, st_mode=0o41777)
    with pytest.raises(ValueError, match="untrusted ancestor"):
        report.check_ancestor(untrusted)
    report.check_ancestor(SimpleNamespace(st_uid=0, st_mode=0o41777))


def test_unknown_child_exit_status_is_not_success(report, monkeypatch):
    from types import SimpleNamespace

    monkeypatch.setattr(
        report.RunningProcess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(returncode=None, stdout=b"{}", stderr=b""),
    )
    with pytest.raises(RuntimeError, match="GitHub request failed"):
        report.github_get("repos/o/r/actions/jobs/1")


def test_malformed_cli_argument_is_bounded(report, capsys):
    assert report.main(["--run", "9" * 100000]) != 0
    error = capsys.readouterr().err
    assert len(error.encode()) < 1024
    assert "ci_failure_report:" in error
