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
