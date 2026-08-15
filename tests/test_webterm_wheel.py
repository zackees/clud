import base64
import hashlib
import zipfile

from ci.webterm_wheel import add_companion, companion_name, desktop_target, wheel_has_companion


def test_desktop_target_recognizes_only_desktop_wheels() -> None:
    assert desktop_target("x86_64-pc-windows-msvc")
    assert desktop_target("aarch64-apple-darwin")
    assert not desktop_target("x86_64-unknown-linux-gnu")


def test_add_companion_updates_record_and_replaces_old_binary(tmp_path) -> None:
    wheel = tmp_path / "clud-2.7.1-py3-none-win_amd64.whl"
    companion = tmp_path / companion_name("x86_64-pc-windows-msvc")
    companion.write_bytes(b"new-webterm")
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr("clud-2.7.1.dist-info/WHEEL", "Wheel-Version: 1.0\n")
        archive.writestr("clud-2.7.1.data/scripts/clud-webterm.exe", b"old-webterm")
        archive.writestr("clud-2.7.1.dist-info/RECORD", "stale")

    add_companion(wheel, companion, "x86_64-pc-windows-msvc")

    with zipfile.ZipFile(wheel) as archive:
        script = "clud-2.7.1.data/scripts/clud-webterm.exe"
        assert archive.read(script) == b"new-webterm"
        record = archive.read("clud-2.7.1.dist-info/RECORD").decode()
    digest = base64.urlsafe_b64encode(hashlib.sha256(b"new-webterm").digest()).rstrip(b"=").decode()
    assert f"{script},sha256={digest},11" in record
    assert wheel_has_companion(wheel, "x86_64-pc-windows-msvc")
