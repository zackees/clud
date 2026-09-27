"""The release can publish only six correctly typed native downloads."""

from __future__ import annotations

import struct

import pytest

from installer.verify_native_assets import TARGETS, verify, verify_one


def native_payload(kind: str, arch: str) -> bytes:
    payload = bytearray(256)
    if kind == "elf":
        payload[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<HH", payload, 16, 2, {"x86_64": 62, "aarch64": 183}[arch])
        struct.pack_into("<Q", payload, 32, 64)
        struct.pack_into("<HH", payload, 54, 56, 1)
        struct.pack_into("<I", payload, 64, 1)
    elif kind == "pe":
        payload[:2] = b"MZ"
        struct.pack_into("<I", payload, 0x3C, 128)
        payload[128:132] = b"PE\0\0"
        struct.pack_into("<H", payload, 132, {"x86_64": 0x8664, "aarch64": 0xAA64}[arch])
    else:
        payload[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", payload, 4, {"x86_64": 0x01000007, "aarch64": 0x0100000C}[arch])
    return bytes(payload)


def test_six_native_downloads_have_exact_names_and_architectures(tmp_path) -> None:
    for suffix, (kind, arch) in TARGETS.items():
        (tmp_path / f"clud-2.9.0-{suffix}").write_bytes(native_payload(kind, arch))
    assert len(verify(tmp_path, "2.9.0")) == 6
    (tmp_path / "clud-2.9.0-aarch64-unknown-linux-musl").write_bytes(
        native_payload("elf", "x86_64")
    )
    with pytest.raises(ValueError, match="architecture"):
        verify(tmp_path, "2.9.0")
    (tmp_path / "clud-2.9.0-aarch64-unknown-linux-musl").rename(tmp_path / "unexpected-asset")
    with pytest.raises(ValueError, match="missing"):
        verify(tmp_path, "2.9.0")


def test_uploaded_arm_binary_is_verified_before_execution(tmp_path) -> None:
    path = tmp_path / "clud-2.9.0-aarch64-unknown-linux-musl"
    path.write_bytes(native_payload("elf", "aarch64"))
    assert verify_one(tmp_path, "2.9.0", "aarch64-unknown-linux-musl")[0] == 256
    path.write_bytes(native_payload("elf", "x86_64"))
    with pytest.raises(ValueError, match="architecture"):
        verify_one(tmp_path, "2.9.0", "aarch64-unknown-linux-musl")
