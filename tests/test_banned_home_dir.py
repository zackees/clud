"""#1836: the home-directory guard flags every second resolver and nothing else."""

from __future__ import annotations

from ci.banned_home_dir import RESOLVER, is_scanned, main, scan

SRC = "crates/clud-bin/src/example.rs"


def rules(rel: str, text: str) -> list[str]:
    return [rule for _number, rule, _line in scan(rel, text)]


def test_dirs_home_dir_is_banned_called_or_passed() -> None:
    assert rules(SRC, "let h = dirs::home_dir();") == ["home_dir call"]
    assert rules(SRC, "x.or_else(dirs::home_dir)") == ["home_dir call"]
    assert rules(SRC, "let h = std::env::home_dir();") == ["home_dir call"]


def test_raw_userprofile_read_is_banned() -> None:
    assert rules(SRC, 'std::env::var_os("USERPROFILE")') == ["raw USERPROFILE read"]
    assert rules(SRC, 'env::var( "USERPROFILE" ).ok()') == ["raw USERPROFILE read"]


def test_the_resolver_and_canonical_calls_are_allowed() -> None:
    assert rules(RESOLVER, 'dirs::home_dir(); var_os("USERPROFILE")') == []
    assert rules(SRC, "let h = crate::home::user_home();") == []


def test_comments_and_test_modules_do_not_trip_it() -> None:
    assert rules(SRC, "// never call dirs::home_dir() here") == []
    text = '#[cfg(test)]\nmod tests {\n    let p = std::env::var("USERPROFILE");\n}\n'
    assert rules(SRC, text) == []


def test_a_mid_file_cfg_test_helper_does_not_hide_later_code() -> None:
    text = "#[cfg(test)]\nfn helper() {}\nfn real() { dirs::home_dir(); }\n"
    assert rules(SRC, text) == ["home_dir call"]


def test_test_files_and_vendor_are_not_scanned() -> None:
    assert not is_scanned("crates/clud-bin/src/foo_tests.rs")
    assert not is_scanned("crates/clud-bin/tests/it.rs")
    assert not is_scanned("vendor/dirs/src/lib.rs")
    assert is_scanned(SRC)


def test_the_tree_is_clean() -> None:
    assert main() == 0
