"""Guard: every manually-run ignored E2E probe is listed in ci.md (issue #1368).

Probes marked ``#[ignore]`` with a "run manually" note never execute in CI, so
the only thing keeping them alive is the manual checklist in
``docs/architecture/ci.md``. This test fails when a new probe is added without
a checklist entry, or when a checklist entry points at a file that is gone.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
TESTS_DIR = REPO_ROOT / "crates" / "clud-bin" / "tests"
CI_DOC = REPO_ROOT / "docs" / "architecture" / "ci.md"
MARKER = "<!-- manual-windows-probes -->"
COMMAND_RE = re.compile(r"soldr cargo test -p clud --test (\S+) (\S+) -- --ignored")


def _discover_manual_probes() -> dict[str, tuple[str, Path]]:
    """Map file stem -> (test target, path) for manually-run ignored probes."""
    probes: dict[str, tuple[str, Path]] = {}
    for path in sorted(TESTS_DIR.rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        if "#[ignore" not in text or "run manually" not in text.lower():
            continue
        rel = path.relative_to(TESTS_DIR)
        target = rel.parts[0] if len(rel.parts) > 1 else path.stem
        probes[path.stem] = (target, path)
    return probes


def _expected_command(target: str, stem: str) -> str:
    return f"soldr cargo test -p clud --test {target} {stem} -- --ignored"


def test_every_run_manually_ignored_test_is_in_the_checklist() -> None:
    doc = CI_DOC.read_text(encoding="utf-8")
    assert MARKER in doc, (
        f"{CI_DOC.relative_to(REPO_ROOT)} is missing the {MARKER} checklist "
        "section for manually-run Windows probes (issue #1368)."
    )
    missing = []
    for stem, (target, path) in _discover_manual_probes().items():
        command = _expected_command(target, stem)
        if command not in doc:
            missing.append(f"{path.relative_to(REPO_ROOT)}: expected `{command}`")
    assert not missing, (
        "Manually-run ignored probes are not listed in "
        f"{CI_DOC.relative_to(REPO_ROOT)} (see issue #1368):\n  "
        + "\n  ".join(missing)
    )


def test_known_probes_are_discovered() -> None:
    discovered = set(_discover_manual_probes())
    expected = {"wedge_watchdog_e2e", "win32_hooking_probe", "tier_refresh_probe"}
    assert expected <= discovered, (
        f"scanner missed known probes {sorted(expected - discovered)}; "
        "the #[ignore]/'run manually' heuristic may have drifted (issue #1368)."
    )


def test_checklist_commands_point_at_existing_files() -> None:
    doc = CI_DOC.read_text(encoding="utf-8")
    stale = []
    for target, stem in COMMAND_RE.findall(doc):
        if not (TESTS_DIR / target / f"{stem}.rs").is_file():
            stale.append(f"--test {target} {stem}")
    assert not stale, (
        f"{CI_DOC.relative_to(REPO_ROOT)} lists probes with no source file "
        f"under {TESTS_DIR.relative_to(REPO_ROOT)} (issue #1368): {stale}"
    )
