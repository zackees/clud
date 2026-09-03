"""Unit tests for ci/check_wheel_size.py.

Issue #1017: three releases were tagged, built green, and never published,
because a linux wheel had grown past PyPI's 100 MB per-file limit and
nothing measured it. These synthesize wheels of a known size and drive the
same entry point the CI step uses.
"""

from __future__ import annotations

import zipfile
from pathlib import Path

from ci.check_wheel_size import (
    PYPI_FILE_LIMIT_MB,
    RELEASE_MAX_MB,
    collect_wheels,
    main,
    wheel_size_mb,
)


def _wheel(path: Path, payload_bytes: int) -> Path:
    """A real zip of roughly `payload_bytes`, stored uncompressed.

    ZIP_STORED because the check measures the file on disk, which is what
    PyPI measures; compressing random-free filler would make the on-disk
    size unrelated to the number the test asks for.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_STORED) as zf:
        zf.writestr("clud/_payload.bin", b"\0" * payload_bytes)
    return path


def test_the_release_cap_sits_under_the_pypi_limit() -> None:
    """The cap has to leave headroom, or it is just a second way to fail.

    Equality would mean the check passes a wheel PyPI then refuses."""
    assert RELEASE_MAX_MB < PYPI_FILE_LIMIT_MB


def test_size_is_measured_on_disk_not_uncompressed(tmp_path: Path) -> None:
    """PyPI's limit is on the uploaded file, so that is what gets measured."""
    wheel = _wheel(tmp_path / "clud-0-py3-none-any.whl", 2 * 1024 * 1024)

    measured = wheel_size_mb(wheel)

    assert 2.0 <= measured < 2.1, measured


def test_a_wheel_under_the_cap_passes(tmp_path: Path, capsys) -> None:
    _wheel(tmp_path / "clud-0-py3-none-win_amd64.whl", 1024 * 1024)

    assert main(["--dist-dir", str(tmp_path), "--max-mb", "5"]) == 0
    assert "OK:" in capsys.readouterr().err


def test_a_wheel_over_the_cap_fails_and_names_both_numbers(
    tmp_path: Path, capsys
) -> None:
    """A size failure has to be actionable from the log alone.

    #1017's failure said nothing at all -- the upload just errored and the
    release job was skipped. Naming the wheel and both numbers is the whole
    point of the check."""
    _wheel(tmp_path / "clud-0-py3-none-manylinux_2_17_x86_64.whl", 4 * 1024 * 1024)

    assert main(["--dist-dir", str(tmp_path), "--max-mb", "3"]) == 1

    err = capsys.readouterr().err
    assert "manylinux_2_17_x86_64" in err
    assert "4.0 MB" in err
    assert "3 MB" in err
    assert str(PYPI_FILE_LIMIT_MB) in err


def test_without_a_cap_it_reports_but_never_fails(tmp_path: Path, capsys) -> None:
    """The PR lane builds dev wheels, whose size says nothing about the
    release wheel's. Reporting is useful there; enforcing a release-shaped
    number against an unmeasured one is not."""
    _wheel(tmp_path / "clud-0-py3-none-any.whl", 8 * 1024 * 1024)

    assert main(["--dist-dir", str(tmp_path)]) == 0

    err = capsys.readouterr().err
    assert "8.0 MB" in err, err
    assert "nothing" in err, err
    assert "enforced" in err, err


def test_every_wheel_is_reported_largest_first(tmp_path: Path, capsys) -> None:
    """The 7x gap between linux and Windows is what made #1017 obvious in
    hindsight. It is only visible if every wheel is listed together."""
    _wheel(tmp_path / "clud-0-py3-none-win_amd64.whl", 1024 * 1024)
    _wheel(tmp_path / "clud-0-py3-none-manylinux_2_17_x86_64.whl", 3 * 1024 * 1024)
    _wheel(tmp_path / "clud-0-py3-none-macosx_11_0_arm64.whl", 2 * 1024 * 1024)

    assert main(["--dist-dir", str(tmp_path), "--max-mb", "10"]) == 0

    # Report lines only -- the trailing "OK: largest wheel ..." summary also
    # names a wheel, and counting it would hide a missing row.
    lines = [
        ln
        for ln in capsys.readouterr().err.splitlines()
        if ln.startswith("  ") and ln.strip().endswith(".whl")
    ]
    assert len(lines) == 3, lines
    assert "manylinux" in lines[0], lines
    assert "win_amd64" in lines[2], lines


def test_no_wheels_is_not_a_failure(tmp_path: Path) -> None:
    """A lane that built no wheel has nothing to check, matching
    `check_windows_wheel`. Failing here would break every non-wheel job."""
    assert main(["--dist-dir", str(tmp_path), "--max-mb", "1"]) == 0


def test_a_named_wheel_that_is_missing_fails(tmp_path: Path, capsys) -> None:
    """Distinct from "no wheels": the caller named a file that is not there,
    which usually means the build step silently produced nothing."""
    assert main([str(tmp_path / "absent.whl"), "--max-mb", "90"]) == 1
    assert "not a file" in capsys.readouterr().err


def test_explicit_paths_and_dist_dir_are_deduped(tmp_path: Path) -> None:
    wheel = _wheel(tmp_path / "clud-0-py3-none-any.whl", 1024)

    assert collect_wheels([wheel], tmp_path) == [wheel]
