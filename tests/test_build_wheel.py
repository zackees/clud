import zipfile

from ci import build_wheel


def test_windows_soldr_wheel_packages_prebuilt_executables(tmp_path):
    target_dir = tmp_path / "target"
    binaries = target_dir / "x86_64-pc-windows-msvc" / "release"
    binaries.mkdir(parents=True)
    for name in build_wheel.REQUIRED_SCRIPTS:
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
        for name in build_wheel.REQUIRED_SCRIPTS:
            assert f"clud-2.5.4.data/scripts/{name}.exe" in members
        assert "clud-2.5.4.dist-info/METADATA" in members
        assert "clud-2.5.4.dist-info/WHEEL" in members
        assert "clud-2.5.4.dist-info/RECORD" in members


def test_windows_wheel_ships_the_cmd_scan_binary() -> None:
    """#862: 2.5.5 shipped the hook rollout pointing configs at
    `clud-cmd-scan` while the hand-packed win_amd64 wheel didn't contain the
    binary — every Bash PreToolUse call on Windows errored `command not
    found`, and the scan protection was silently off."""
    assert "clud-cmd-scan" in build_wheel.REQUIRED_SCRIPTS


def test_local_webterm_companion_uses_the_configured_target_directory(
    monkeypatch, tmp_path
) -> None:
    target = "x86_64-pc-windows-msvc"
    companion = tmp_path / "clud-webterm" / "target" / target / "debug" / "clud-webterm.exe"
    companion.parent.mkdir(parents=True)
    companion.write_bytes(b"webterm")
    monkeypatch.setattr(build_wheel, "ROOT", tmp_path)

    class Result:
        returncode = 0

    monkeypatch.setattr(build_wheel.process, "run", lambda *args, **kwargs: Result())

    assert build_wheel.build_local_webterm_companion(mode="dev", target=target, env={}) == companion


def test_hook_rollout_target_is_a_shipped_script() -> None:
    """Whatever binary the rollout migrates hook configs to MUST be in the
    wheel. Reads NEW_COMMAND from the rollout source so a future rename
    (bad-cmd -> cmd-scan -> ...) cannot repeat #862: the rename lands, this
    fails until REQUIRED_SCRIPTS is extended too."""
    import re

    source = (
        build_wheel.ROOT / "crates" / "clud-bin" / "src" / "block_bad_cmd_rollout.rs"
    ).read_text(encoding="utf-8")
    match = re.search(r'const NEW_COMMAND: &str = "([^"]+)"', source)
    assert match, "NEW_COMMAND not found in block_bad_cmd_rollout.rs"
    assert match.group(1) in build_wheel.REQUIRED_SCRIPTS, (
        f"hook rollout targets `{match.group(1)}` but the wheel does not ship it"
    )


def test_required_scripts_are_declared_crate_binaries() -> None:
    """Every shipped script must be a real `[[bin]]` — a typo here would make
    the Windows packer fail at release time instead of test time."""
    cargo = (build_wheel.ROOT / "crates" / "clud-bin" / "Cargo.toml").read_text(
        encoding="utf-8"
    )
    import re

    declared = set(re.findall(r'^name = "(clud[^"]*)"', cargo, re.MULTILINE))
    for name in build_wheel.REQUIRED_SCRIPTS:
        assert name in declared, f"{name} is not a declared [[bin]] in clud-bin"


def test_verify_windows_wheel_scripts_uses_target_not_host(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name in build_wheel.REQUIRED_SCRIPTS:
            archive.writestr(f"clud-2.3.0.data/scripts/{name}.exe", b"")
        archive.writestr("clud-2.3.0.data/scripts/clud-webterm.exe", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 0


def test_verify_windows_wheel_scripts_requires_the_webterm_companion(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-win_amd64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name in build_wheel.REQUIRED_SCRIPTS:
            archive.writestr(f"clud-2.3.0.data/scripts/{name}.exe", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 1


def test_verify_macos_wheel_scripts_requires_the_webterm_companion(monkeypatch, tmp_path):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")
    wheel = tmp_path / "clud-2.3.0-py3-none-macosx_11_0_arm64.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for name in build_wheel.REQUIRED_SCRIPTS:
            archive.writestr(f"clud-2.3.0.data/scripts/{name}", b"")

    assert build_wheel.verify_wheel_scripts(wheel) == 1


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
