"""Contract tests for the Dylint tool, library, and nightly pins.

Issue #911 normalized clud onto a supported Dylint and deleted the recovery
scaffolding that earlier 6.0.x releases needed. These tests guard both halves:
the version/nightly lockstep, and the *absence* of the scaffolding — the
latter matters because the failure it worked around is intermittent, so a
future debugging session could plausibly re-add the crutch instead of moving
the pin.
"""

from __future__ import annotations

import re
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent
DYLINT_VERSION = "6.0.4"
DYLINT_NIGHTLY = "nightly-2026-04-16"
LINT_DIR = ROOT / "dylints" / "ban_manual_slash_normalize"
WORKFLOW = ROOT / ".github" / "workflows" / "_dylint.yml"


def _toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def _workflow_text() -> str:
    return WORKFLOW.read_text(encoding="utf-8")


def test_dylint_stack_versions_stay_in_lockstep() -> None:
    lint_manifest = _toml(LINT_DIR / "Cargo.toml")
    toolchain = _toml(LINT_DIR / "rust-toolchain.toml")
    workflow = _workflow_text()

    assert lint_manifest["dependencies"]["dylint_linting"] == DYLINT_VERSION
    assert toolchain["toolchain"]["channel"] == DYLINT_NIGHTLY
    assert (
        f"cargo install cargo-dylint dylint-link --version {DYLINT_VERSION} --locked"
        in workflow
    )
    # Every nightly the workflow names must be the one the lint crate pins;
    # a stray second date means the driver and the lints disagree.
    assert set(re.findall(r"nightly-\d{4}-\d{2}-\d{2}", workflow)) == {DYLINT_NIGHTLY}

    # The README tells developers what to install locally. Leaving it out of
    # the lockstep is how you get a green suite that hands people a
    # mismatched cargo-dylint — the tool/library skew #911 was about.
    readme = (ROOT / "dylints" / "README.md").read_text(encoding="utf-8")
    assert (
        f"cargo install cargo-dylint dylint-link --version {DYLINT_VERSION} --locked"
        in readme
    )
    assert set(re.findall(r"nightly-\d{4}-\d{2}-\d{2}", readme)) == {DYLINT_NIGHTLY}


def test_dylint_lockfile_matches_the_pinned_version() -> None:
    """The lockfile is committed, so it can drift from Cargo.toml silently."""
    lock = (LINT_DIR / "Cargo.lock").read_text(encoding="utf-8")
    assert f'name = "dylint_linting"\nversion = "{DYLINT_VERSION}"' in lock


def test_dylint_workflow_runs_one_plain_invocation() -> None:
    """No retry, no alias reconstruction, no hand-built driver (issue #911)."""
    workflow = _workflow_text()

    # Exactly one `cargo dylint` run — the recovery path ran it twice.
    assert workflow.count("cargo dylint --all") == 1

    # The specific scaffolding that was removed. Each of these appearing again
    # means someone restored the workaround instead of moving the version pin.
    for banned in (
        "build_dylint_driver",
        "Build published Dylint driver",
        "Set up Dylint Python environment",
        "VENV_PY",
        "missing-alias",
        "libban_manual_slash_normalize@",
        "set +e",
        "for attempt in",
    ):
        assert banned not in workflow, (
            f"{banned!r} is Dylint recovery scaffolding removed in #911; "
            "if Dylint regressed, move the version pin instead of restoring it"
        )


def test_dylint_driver_builder_is_gone() -> None:
    """The hand-built-driver script was retired with the workaround."""
    assert not (ROOT / "ci" / "build_dylint_driver.py").exists()


def test_lint_crate_links_through_dylint_link() -> None:
    """The actual fix behind #911's removed scaffolding.

    `dylint-link` names the cdylib `lib<name>@<toolchain>.so`, which is the
    filename `cargo dylint` looks up. Without this config the build succeeds
    and emits a plain `lib<name>.so`, and Dylint fails with "Could not find
    ... despite successful build" — the failure CI used to paper over by
    copying the artifact and retrying. Verified against Dylint 6.0.4, so this
    is not something a version bump makes redundant.
    """
    config = LINT_DIR / ".cargo" / "config.toml"
    assert config.exists(), "lint crate must configure the dylint-link linker"
    # Parsed, not substring-matched: the file carries a long comment *about*
    # the directive, so `"linker=dylint-link" in text` would still pass with
    # the directive commented out — resurrecting the exact failure this
    # guards. `cfg(all())` rather than a concrete triple so it applies on
    # whatever host builds the lint.
    parsed = _toml(config)
    assert parsed["target"]["cfg(all())"]["rustflags"] == ["-C", "linker=dylint-link"]
