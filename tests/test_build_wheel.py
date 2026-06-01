from ci import build_wheel


def test_linux_release_uses_zigbuild_linker(monkeypatch):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")

    env = build_wheel.build_environment("release", {"SOLDR_LINKER": "fast"})

    assert env["SOLDR_LINKER"] == "default"


def test_linux_dev_keeps_existing_linker(monkeypatch):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Linux")

    env = build_wheel.build_environment("dev", {"SOLDR_LINKER": "fast"})

    assert env["SOLDR_LINKER"] == "fast"


def test_non_linux_release_keeps_existing_linker(monkeypatch):
    monkeypatch.setattr(build_wheel.platform, "system", lambda: "Darwin")

    env = build_wheel.build_environment("release", {"SOLDR_LINKER": "fast"})

    assert env["SOLDR_LINKER"] == "fast"
