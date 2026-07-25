"""Contract tests for the Dylint tool, library, and nightly pins."""

from __future__ import annotations

import re
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent
DYLINT_VERSION = "6.0.1"
DYLINT_NIGHTLY = "nightly-2026-04-16"
LINT_DIR = ROOT / "dylints" / "ban_manual_slash_normalize"


def _toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def test_dylint_stack_versions_stay_in_lockstep() -> None:
    lint_manifest = _toml(LINT_DIR / "Cargo.toml")
    toolchain = _toml(LINT_DIR / "rust-toolchain.toml")
    workflow = (ROOT / ".github" / "workflows" / "dylint.yml").read_text(
        encoding="utf-8"
    )

    assert lint_manifest["dependencies"]["dylint_linting"] == DYLINT_VERSION
    assert toolchain["toolchain"]["channel"] == DYLINT_NIGHTLY
    assert (
        f"cargo install cargo-dylint dylint-link --version {DYLINT_VERSION} --locked"
        in workflow
    )
    assert set(re.findall(r"nightly-\d{4}-\d{2}-\d{2}", workflow)) == {
        DYLINT_NIGHTLY
    }


def test_dylint_workflow_has_no_legacy_driver_or_generic_retry_loop() -> None:
    workflow = (ROOT / ".github" / "workflows" / "dylint.yml").read_text(
        encoding="utf-8"
    )

    assert "build_dylint_driver.py" not in workflow
    assert "DYLINT_DRIVER_PATH" not in workflow
    assert "for attempt in" not in workflow
    assert workflow.count("cargo dylint --all") == 2
    assert "Dylint 6.0.1 can build the cdylib successfully" in workflow
