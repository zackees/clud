"""Guards for `ci/harness_budget.py`: the `clud` package stays at two harnesses (#1726)."""

from __future__ import annotations

from pathlib import Path

from ci import harness_budget as lint


def _write(root: Path, rel: str, text: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _workspace(root: Path, manifest: str = "") -> None:
    _write(root, "Cargo.toml", '[workspace]\nmembers = ["crates/clud-bin", "other"]\n')
    _write(
        root,
        "crates/clud-bin/Cargo.toml",
        '[package]\nname = "clud"\nversion = "0.1.0"\n' + manifest,
    )
    _write(root, "crates/clud-bin/src/lib.rs", "#[test]\nfn t() {}\n")
    _write(root, "crates/clud-bin/tests/integration/main.rs", "#[test]\nfn t() {}\n")
    _write(root, "other/Cargo.toml", '[package]\nname = "other"\nversion = "0.1.0"\n')
    _write(root, "other/tests/a.rs", "#[test]\nfn t() {}\n")
    _write(root, "other/tests/b.rs", "#[test]\nfn t() {}\n")


def _flagged(root: Path) -> list[tuple[str, str]]:
    return sorted((t.kind, t.name) for t in lint.extra_harnesses(root))


def test_the_real_workspace_keeps_two_clud_harnesses() -> None:
    assert lint.extra_harnesses() == []


def test_lib_and_one_integration_harness_pass_and_other_packages_are_free(
    tmp_path: Path,
) -> None:
    _workspace(tmp_path)
    assert _flagged(tmp_path) == []


def test_a_second_integration_target_is_flagged(tmp_path: Path) -> None:
    _workspace(tmp_path)
    _write(tmp_path, "crates/clud-bin/tests/reaper/main.rs", "#[test]\nfn t() {}\n")
    _write(tmp_path, "crates/clud-bin/tests/smoke.rs", "#[test]\nfn t() {}\n")
    assert _flagged(tmp_path) == [("test", "reaper"), ("test", "smoke")]


def test_a_bin_with_tests_is_flagged_until_test_false(tmp_path: Path) -> None:
    _workspace(
        tmp_path,
        '[[bin]]\nname = "helper"\npath = "src/bin/helper.rs"\n',
    )
    _write(tmp_path, "crates/clud-bin/src/bin/helper.rs", "fn main() {}\n#[test]\nfn t() {}\n")
    assert _flagged(tmp_path) == [("bin", "helper")]
    _workspace(
        tmp_path,
        '[[bin]]\nname = "helper"\npath = "src/bin/helper.rs"\ntest = false\n',
    )
    assert _flagged(tmp_path) == []
