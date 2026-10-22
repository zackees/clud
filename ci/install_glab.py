"""Install a checksum-pinned real GitLab CLI for native secret acceptance."""
from __future__ import annotations

import argparse
import hashlib
import io
import stat
import tarfile
import urllib.request
import zipfile
from pathlib import Path, PurePosixPath

VERSION = "1.124.0"
BASE_URL = f"https://gitlab.com/gitlab-org/cli/-/releases/v{VERSION}/downloads/"
MAX_BYTES = 128 * 1024 * 1024
# Official release checksums: downloads/checksums.txt at the pinned release.
ASSETS = {
    "x86_64-unknown-linux-gnu": (
        "glab_1.124.0_linux_amd64.tar.gz",
        "63df684842b0697e418649dd64374d37ac1da0fbcdcb422d6a5557222f8bb40f",
    ),
    "aarch64-unknown-linux-gnu": (
        "glab_1.124.0_linux_arm64.tar.gz",
        "0716395a0efeb991fc844b33fe55310645afcbd9b9746e50c725e9d1cd1b8d16",
    ),
    "x86_64-apple-darwin": (
        "glab_1.124.0_darwin_amd64.tar.gz",
        "6adb7155566bedd6272d8f286e6c75b080d0050a349de4bed79661226d10d954",
    ),
    "aarch64-apple-darwin": (
        "glab_1.124.0_darwin_arm64.tar.gz",
        "04aad4d989d98cb030318e09359f02e0ba2637ede682ec0dee1f5974dedd44c1",
    ),
    "x86_64-pc-windows-msvc": (
        "glab_1.124.0_windows_amd64.zip",
        "9b58b54cf841b43215ef01ba90793d8a845e81ddb7b3dfa2b5a238cde4b77b34",
    ),
    "aarch64-pc-windows-msvc": (
        "glab_1.124.0_windows_arm64.zip",
        "487a33c9cf148bbfa313ee359e5ecc34284936754a3348dfe86ba350bed78bb3",
    ),
}


def download(name: str) -> bytes:
    with urllib.request.urlopen(BASE_URL + name, timeout=60) as response:
        if not response.geturl().startswith("https://"):
            raise ValueError("GitLab CLI download must remain HTTPS")
        payload = response.read(MAX_BYTES + 1)
    if len(payload) > MAX_BYTES:
        raise ValueError("GitLab CLI archive exceeds the download limit")
    return payload


def zip_binary(payload: bytes, binary: str) -> bytes:
    with zipfile.ZipFile(io.BytesIO(payload)) as archive:
        matches = [
            entry for entry in archive.infolist()
            if not entry.is_dir()
            and PurePosixPath(entry.filename.replace("\\", "/")).name == binary
        ]
        if len(matches) != 1:
            raise ValueError("GitLab CLI archive must contain exactly one executable")
        entry = matches[0]
        mode = stat.S_IFMT(entry.external_attr >> 16)
        if mode not in (0, stat.S_IFREG) or entry.file_size > MAX_BYTES:
            raise ValueError("GitLab CLI executable is not an allowed regular file")
        with archive.open(entry) as stream:
            return stream.read(MAX_BYTES + 1)


def tar_binary(payload: bytes, binary: str) -> bytes:
    with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
        matches = [entry for entry in archive.getmembers()
                   if entry.isfile() and PurePosixPath(entry.name).name == binary]
        if len(matches) != 1 or matches[0].size > MAX_BYTES:
            raise ValueError("GitLab CLI archive must contain one bounded regular executable")
        stream = archive.extractfile(matches[0])
        if stream is None:
            raise ValueError("GitLab CLI executable cannot be read")
        with stream:
            return stream.read(MAX_BYTES + 1)


def verified_binary(payload: bytes, name: str, digest: str) -> tuple[str, bytes]:
    if len(payload) > MAX_BYTES:
        raise ValueError("GitLab CLI archive exceeds the download limit")
    if hashlib.sha256(payload).hexdigest() != digest:
        raise ValueError("GitLab CLI archive checksum mismatch")
    binary = "glab.exe" if name.endswith(".zip") else "glab"
    data = zip_binary(payload, binary) if name.endswith(".zip") else tar_binary(payload, binary)
    if not data or len(data) > MAX_BYTES:
        raise ValueError("GitLab CLI executable size is invalid")
    return binary, data


def install(target: str, destination: Path) -> Path:
    if target not in ASSETS:
        raise ValueError(f"unsupported GitLab CLI acceptance target: {target}")
    name, digest = ASSETS[target]
    binary, data = verified_binary(download(name), name, digest)
    destination.mkdir(parents=True, exist_ok=True)
    output = destination / binary
    output.write_bytes(data)
    output.chmod(0o755)
    return output.resolve()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=sorted(ASSETS))
    parser.add_argument("--destination", required=True, type=Path)
    parser.add_argument("--github-env", required=True, type=Path)
    args = parser.parse_args()
    executable = install(args.target, args.destination)
    with args.github_env.open("a", encoding="utf-8", newline="\n") as stream:
        stream.write(f"CLUD_TEST_GLAB_BINARY={executable}\n")
    print(f"Verified GitLab CLI {VERSION} for {args.target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
