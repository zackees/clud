"""Guards for `ci/banned_empty_harnesses.py`: no test harness without tests (#1714)."""

from __future__ import annotations

from pathlib import Path

from ci import banned_empty_harnesses as lint


def _write(root: Path, rel: str, text: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _workspace(root: Path, manifest: str) -> None:
    _write(root, "Cargo.toml", '[workspace]\nmembers = ["pkg"]\n')
    _write(root, "pkg/Cargo.toml", '[package]\nname = "pkg"\nversion = "0.1.0"\n' + manifest)


def _flagged(root: Path) -> list[tuple[str, str]]:
    return [(t.kind, t.name) for t in lint.empty_harnesses(root)]


def test_the_real_workspace_has_no_empty_harness() -> None:
    assert lint.empty_harnesses() == []


def test_flags_bin_lib_and_integration_test_without_tests(tmp_path: Path) -> None:
    _workspace(tmp_path, "")
    _write(tmp_path, "pkg/src/main.rs", "fn main() {}\n")
    _write(tmp_path, "pkg/src/lib.rs", "#[cfg(test)]\nmod nothing {}\n")
    _write(tmp_path, "pkg/tests/it.rs", "// #[test] in a comment does not count\n")
    assert sorted(_flagged(tmp_path)) == [("bin", "pkg"), ("lib", "pkg"), ("test", "it")]


def test_test_false_silences_the_target(tmp_path: Path) -> None:
    _workspace(tmp_path, '[[bin]]\nname = "pkg"\npath = "src/main.rs"\ntest = false\n')
    _write(tmp_path, "pkg/src/main.rs", "fn main() {}\n")
    assert _flagged(tmp_path) == []


def test_tests_found_through_mod_and_path_declarations(tmp_path: Path) -> None:
    _workspace(tmp_path, "")
    _write(tmp_path, "pkg/src/main.rs", "mod a;\nfn main() {}\n")
    _write(tmp_path, "pkg/src/a.rs", '#[path = "elsewhere.rs"]\nmod b;\n')
    _write(
        tmp_path,
        "pkg/src/elsewhere.rs",
        '#[tokio::test(flavor = "current_thread")]\nasync fn t() {}\n',
    )
    _write(tmp_path, "pkg/tests/suite/main.rs", "mod part;\n")
    _write(tmp_path, "pkg/tests/suite/part.rs", "#[test]\nfn t() {}\n")
    assert _flagged(tmp_path) == []
