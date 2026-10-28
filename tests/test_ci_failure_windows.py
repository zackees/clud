"""Windows CI diagnostics fail closed until protected artifacts are supported (#2246)."""

import importlib.util
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows-specific contract")


def test_windows_does_not_create_unprotected_artifact():
    path = (
        Path(__file__).resolve().parents[1]
        / "crates/clud-bin/assets/tools/github/ci_failure_report.py"
    )
    spec = importlib.util.spec_from_file_location("ci_failure_windows", path)
    assert spec
    assert spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    with pytest.raises(ValueError, match="Windows artifact privacy"):
        module.save_artifact(b"sensitive")


def test_windows_cli_refuses_before_downloading(monkeypatch, tmp_path, capsys):
    path = (
        Path(__file__).resolve().parents[1]
        / "crates/clud-bin/assets/tools/github/ci_failure_report.py"
    )
    spec = importlib.util.spec_from_file_location("ci_failure_windows_cli", path)
    assert spec
    assert spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setenv("CLUD_TOOL_OUTPUT_DIR", str(tmp_path))
    monkeypatch.setattr(
        module,
        "github_get",
        lambda *_: b'{"run_id": 123, "status": "completed", "conclusion": "failure"}',
    )

    def no_download(*_args):
        raise AssertionError("Windows must fail before downloading a log")

    monkeypatch.setattr(module, "github_download", no_download)
    assert module.main(["--repo", "o/r", "--run", "123", "--job", "456"]) != 0
    assert "Windows artifact privacy" in capsys.readouterr().err
    assert not list(tmp_path.iterdir())
    assert module.main(["--artifact", str(tmp_path / "ci-failure-12345678.log")]) != 0
    assert "Windows artifact privacy" in capsys.readouterr().err
