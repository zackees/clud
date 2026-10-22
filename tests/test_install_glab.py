"""The CI-only real consumer must be verified before any executable is written."""
from __future__ import annotations

import hashlib
import io
import stat
import tarfile
import zipfile
from pathlib import Path

import pytest

from ci import install_glab


def archive_bytes(kind: str, names: list[str], *, link: bool = False, size: int = 6) -> bytes:
    data = b"binary" if size == 6 else b"x" * size
    stream = io.BytesIO()
    if kind == "zip":
        with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name in names:
                entry = zipfile.ZipInfo(name)
                entry.compress_type = zipfile.ZIP_DEFLATED
                entry.external_attr = ((stat.S_IFLNK if link else stat.S_IFREG) | 0o755) << 16
                archive.writestr(entry, data)
    else:
        with tarfile.open(fileobj=stream, mode="w:gz") as archive:
            for name in names:
                entry = tarfile.TarInfo(name)
                entry.size = 0 if link else len(data)
                if link:
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = "elsewhere"
                archive.addfile(entry, None if link else io.BytesIO(data))
    return stream.getvalue()


@pytest.mark.parametrize(("kind", "binary"), [("tar", "glab"), ("zip", "glab.exe")])
def test_selects_only_the_verified_executable(kind: str, binary: str) -> None:
    payload = archive_bytes(kind, [f"bin/{binary}", "../../unrelated"])
    name = "fixture.zip" if kind == "zip" else "fixture.tar.gz"
    result = install_glab.verified_binary(payload, name, hashlib.sha256(payload).hexdigest())
    assert result == (binary, b"binary")


@pytest.mark.parametrize(("kind", "binary"), [("tar", "glab"), ("zip", "glab.exe")])
@pytest.mark.parametrize("problem", ["missing", "duplicate", "link"])
def test_refuses_ambiguous_or_nonregular_executables(kind: str, binary: str, problem: str) -> None:
    names = [f"bin/{binary}"]
    if problem == "missing":
        names = ["README"]
    elif problem == "duplicate":
        names.append(f"another/{binary}")
    payload = archive_bytes(kind, names, link=problem == "link")
    name = "fixture.zip" if kind == "zip" else "fixture.tar.gz"
    with pytest.raises(ValueError, match="GitLab CLI (archive|executable)"):
        install_glab.verified_binary(payload, name, hashlib.sha256(payload).hexdigest())


def test_checksum_failure_has_no_filesystem_writes(tmp_path: Path, monkeypatch) -> None:
    payload = archive_bytes("tar", ["bin/glab"])
    monkeypatch.setitem(install_glab.ASSETS, "fixture", ("fixture.tar.gz", "0" * 64))
    monkeypatch.setattr(install_glab, "download", lambda _name: payload)
    destination = tmp_path / "consumer"
    with pytest.raises(ValueError, match="checksum mismatch"):
        install_glab.install("fixture", destination)
    assert not destination.exists()


@pytest.mark.parametrize(("kind", "binary"), [("tar", "glab"), ("zip", "glab.exe")])
def test_extracted_executable_is_bounded(kind: str, binary: str, monkeypatch) -> None:
    payload = archive_bytes(kind, [binary], size=10_000)
    monkeypatch.setattr(install_glab, "MAX_BYTES", 256)
    assert len(payload) < install_glab.MAX_BYTES
    name = "fixture.zip" if kind == "zip" else "fixture.tar.gz"
    with pytest.raises(ValueError, match="GitLab CLI (archive|executable)"):
        install_glab.verified_binary(payload, name, hashlib.sha256(payload).hexdigest())


def test_unknown_target_does_not_download(tmp_path: Path, monkeypatch) -> None:
    def forbidden(_name):
        pytest.fail("unknown target attempted a download")
    monkeypatch.setattr(install_glab, "download", forbidden)
    with pytest.raises(ValueError, match="unsupported"):
        install_glab.install("unknown", tmp_path / "consumer")
