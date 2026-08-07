import zipfile

from ci import build_wheel


def test_windows_soldr_wheel_packages_prebuilt_executables(tmp_path):
    target_dir = tmp_path / "target"
    binaries = target_dir / "x86_64-pc-windows-msvc" / "release"
    binaries.mkdir(parents=True)
    for name in ("clud", "clud-shim", "clud-block-bad-cmd"):
        (binaries / f"{name}.exe").write_bytes(f"{name}-binary".encode())

    wheel = build_wheel.build_windows_wheel_from_binaries(
        target="x86_64-pc-windows-msvc",
        profile="release",
        target_dir=target_dir,
        dist_dir=tmp_path / "dist",
        version="2.5.4",
    )

    with zipfile.ZipFile(wheel) as archive:
        members = set(archive.namelist())
        assert "clud/__init__.py" in members
        assert "clud-2.5.4.data/scripts/clud.exe" in members
        assert "clud-2.5.4.data/scripts/clud-shim.exe" in members
        assert "clud-2.5.4.data/scripts/clud-block-bad-cmd.exe" in members
        assert "clud-2.5.4.dist-info/METADATA" in members
        assert "clud-2.5.4.dist-info/WHEEL" in members
        assert "clud-2.5.4.dist-info/RECORD" in members


def test_local_linux_release_wheel_never_claims_a_manylinux_floor(monkeypatch):
    """#858: zig is retired, so a local release wheel links the HOST glibc.

    It must be tagged `linux` (non-distributable) — claiming manylinux2014
    without the soldr blessed toolchain doing the link would ship a lie, and
    passing `--zig` would fail outright now that ziglang is out of the venv.
    """
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")

    cmd = build_wheel.build_command("release")

    assert "--zig" not in cmd
    assert "manylinux2014" not in cmd
    compat = cmd[cmd.index("--compatibility") + 1]
    assert compat == "linux"


def test_verify_windows_wheel_scripts_uses_target_not_host(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for script in ["clud.exe", "clud-shim.exe", "clud-block-bad-cmd.exe"]:
            archive.writestr(f"clud-2.3.0.data/scripts/{script}", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 0


def test_verify_windows_wheel_scripts_rejects_missing_native_helper(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr("clud-2.3.0.data/scripts/clud.exe", b"")
        archive.writestr("clud-2.3.0.data/scripts/clud-shim.exe", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 1


def test_wheels_changed_since_ignores_stale_wheels(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel, "DIST", tmp_path)
    stale = tmp_path / "clud-2.2.0-py3-none-any.whl"
    stale.write_bytes(b"old")
    before = build_wheel.wheel_snapshot()

    fresh = tmp_path / "clud-2.3.0-py3-none-any.whl"
    fresh.write_bytes(b"new")

    assert build_wheel.wheels_changed_since(before) == [fresh]
