"""Guards for `ci/banned_python3.py`: code calls the interpreter `python`."""

from __future__ import annotations

from ci import banned_python3 as lint


def test_flags_every_spelling_of_the_versioned_name() -> None:
    text = "\n".join(
        [
            "#!/usr/bin/env python3",
            'run(["python3.13", "-m", "x"])',
            "apt-get install python3-pip",
            "C:\\\\py\\\\python3.exe",
        ]
    )
    assert [n for n, _ in lint.scan(text)] == [1, 2, 3, 4]


def test_plain_python_and_lookalikes_pass() -> None:
    text = "\n".join(
        [
            "#!/usr/bin/env python",
            "requires-python = '>=3.11'",
            "cpython3x = 1",
            "python -m pytest",
        ]
    )
    assert lint.scan(text) == []


def test_marker_covers_its_own_line_only() -> None:
    text = "apt-get install python3  # python-name-lint: allow\npython3 -c 1"
    assert [n for n, _ in lint.scan(text)] == [2]


def test_next_line_marker_covers_a_continued_line() -> None:
    text = "\n".join(
        [
            "    # Debian names. python-name-lint: allow-next-line",
            "    && apt-get install python3 \\",
            "    python3 -c 1",
        ]
    )
    # Only the line right after the marker is excused.
    assert [n for n, _ in lint.scan(text)] == [3]


def test_shim_modules_are_exempt_and_vendor_is_skipped() -> None:
    assert not lint.is_scanned("crates/clud-bin/src/shim_resolve.rs")
    assert not lint.is_scanned("vendor/foo/setup.py")
    assert not lint.is_scanned("Cargo.lock")
    assert lint.is_scanned("crates/clud-bin/src/tool_run.rs")
    assert lint.is_scanned(".codex/hooks/check-soldr.py")


def test_error_explains_why() -> None:
    # The message is the whole point: the reader must learn why, and what to do.
    assert "Windows" in lint.REASON
    assert "`python`" in lint.REASON
    assert "python-name-lint: allow" in lint.REASON


def test_repository_is_clean() -> None:
    assert lint.main() == 0
