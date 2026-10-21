"""#1708: credentials go to a sink without entering agent-visible output."""

from pathlib import Path

from tests.test_hello import _run


def test_secret_prompt_exposes_explicit_sinks() -> None:
    result = _run("secret", "--help")
    assert result.returncode == 0, result.stderr
    for option in ["--to-file", "--to-command", "--vault", "--multiline"]:
        assert option in result.stdout


def test_secret_prompt_file_sink_keeps_multiline_value_private(tmp_path: Path) -> None:
    import os
    import stat

    source = tmp_path / "fixture"
    sentinel = "-----BEGIN PRIVATE KEY-----\nFAKE_TEST_KEY\n-----END PRIVATE KEY-----\n"
    source.write_text(sentinel)
    destination = tmp_path / "credential"
    result = _run(
        "secret",
        "--multiline",
        "--to-file",
        str(destination),
        env_overrides={
            "CLUD_INTEGRATION_TESTS": "1",
            "CLUD_TEST_SECRET_PROMPT_FILE": str(source),
        },
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "stored"
    assert "FAKE_TEST_KEY" not in result.stdout + result.stderr
    assert destination.read_text() == sentinel
    if os.name != "nt":
        assert stat.S_IMODE(destination.stat().st_mode) == 0o600


def test_secret_prompt_cancellation_is_distinct(tmp_path: Path) -> None:
    destination = tmp_path / "cancelled"
    result = _run(
        "secret",
        "--to-file",
        str(destination),
        env_overrides={"CLUD_INTEGRATION_TESTS": "1", "CLUD_TEST_SECRET_PROMPT_CANCEL": "1"},
    )
    assert result.returncode == 3, result.stderr
    assert result.stdout.strip() == "cancelled"
    assert not destination.exists()


def test_secret_command_sink_suppresses_echoed_credentials(tmp_path: Path) -> None:
    import sys

    secret = "fake-credential-sentinel"
    source = tmp_path / "fixture"
    source.write_text(secret)
    received = tmp_path / "received"
    script = (
        "import pathlib,sys; value=sys.stdin.read(); "
        "pathlib.Path(sys.argv[1]).write_text(value); "
        "print(value); print(value,file=sys.stderr); sys.exit(int(sys.argv[2]))"
    )
    for exit_code, status in [(0, "stored"), (7, "failed")]:
        result = _run(
            "secret",
            "--to-command",
            sys.executable,
            "-c",
            script,
            str(received),
            str(exit_code),
            env_overrides={
                "CLUD_INTEGRATION_TESTS": "1",
                "CLUD_TEST_SECRET_PROMPT_FILE": str(source),
            },
        )
        assert result.returncode == (0 if exit_code == 0 else 1), result.stderr
        assert result.stdout.strip() == status
        assert secret not in result.stdout + result.stderr
        assert received.read_text() == secret


def test_secret_command_timeout_covers_blocked_stdin(tmp_path: Path) -> None:
    import sys
    import time

    source = tmp_path / "fixture"
    source.write_text("x" * (512 * 1024))
    started = time.monotonic()
    result = _run(
        "secret",
        "--to-command",
        sys.executable,
        "-c",
        "import time; time.sleep(10)",
        env_overrides={
            "CLUD_INTEGRATION_TESTS": "1",
            "CLUD_TEST_SECRET_PROMPT_FILE": str(source),
            "CLUD_TEST_SECRET_PROMPT_TIMEOUT_MS": "100",
        },
    )
    assert result.returncode == 1, result.stderr
    assert result.stdout.strip() == "failed"
    assert time.monotonic() - started < 5


def test_secret_prompt_refuses_missing_or_conflicting_sinks() -> None:
    for arguments in [(), ("--to-file", "x", "--vault", "kimi")]:
        result = _run("secret", *arguments)
        assert result.returncode == 2


def test_askpass_refuses_direct_agent_tool_invocation(tmp_path: Path) -> None:
    import os

    from tests import process

    links = _run("__link-aliases", str(tmp_path))
    assert links.returncode == 0, links.stderr
    alias = tmp_path / ("clud-askpass.exe" if os.name == "nt" else "clud-askpass")
    result = process.run([str(alias), "Password"], capture_output=True, text=True, timeout=10)
    assert result.returncode == 2
    assert result.stdout == ""
    assert "must be invoked by git, ssh or sudo" in result.stderr


def test_git_askpass_receives_secret_through_its_protocol(tmp_path: Path) -> None:
    import os
    import shutil

    import pytest

    from tests import process

    git = shutil.which("git")
    if git is None:
        pytest.skip("git is required for the real askpass protocol probe")
    links = _run("__link-aliases", str(tmp_path))
    assert links.returncode == 0, links.stderr
    source = tmp_path / "fixture"
    source.write_text("fake-askpass-token")
    alias = tmp_path / ("clud-askpass.exe" if os.name == "nt" else "clud-askpass")
    env = {
        **os.environ,
        "GIT_ASKPASS": str(alias),
        "GIT_TERMINAL_PROMPT": "0",
        "CLUD_INTEGRATION_TESTS": "1",
        "CLUD_TEST_SECRET_PROMPT_FILE": str(source),
    }
    result = process.run(
        [git, "-c", "credential.helper=", "credential", "fill"],
        input="protocol=https\nhost=example.invalid\nusername=test\n\n",
        capture_output=True,
        text=True,
        timeout=10,
        env=env,
    )
    assert result.returncode == 0, result.stderr
    # Only this fake fixture test deliberately reads the askpass protocol.
    assert "password=fake-askpass-token" in result.stdout


def test_native_secret_window_can_render_and_deliver(tmp_path: Path) -> None:
    """Exercise the real window when the native runner has a graphical desktop."""
    import os
    import shutil
    import sys

    import pytest

    from tests import process
    from tests.test_hello import CLUD

    source = tmp_path / "fixture"
    source.write_text("FAKE_NATIVE_PRIVATE_KEY\nsecond line\n")
    destination = tmp_path / "received"
    env = {
        **os.environ,
        "CLUD_INTEGRATION_TESTS": "1",
        "CLUD_TEST_SECRET_PROMPT_FILE": str(source),
        "CLUD_TEST_SECRET_PROMPT_GUI": "1",
    }
    argv = [CLUD, "secret", "--multiline", "--to-file", str(destination)]
    if sys.platform == "linux" and not (env.get("DISPLAY") or env.get("WAYLAND_DISPLAY")):
        xvfb = shutil.which("xvfb-run")
        if xvfb is None:
            pytest.skip("a desktop or xvfb-run is required for the native window probe")
        argv = [xvfb, "-a", *argv]
    result = process.run(argv, capture_output=True, text=True, timeout=30, env=env)
    if result.returncode == 1 and "No graphical secret prompt is available" in result.stderr:
        pytest.skip("native runner has no supported graphics context")
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "stored"
    assert "FAKE_NATIVE_PRIVATE_KEY" not in result.stdout + result.stderr
    assert destination.read_text() == source.read_text()


def test_ssh_askpass_unlocks_a_fixture_key(tmp_path: Path) -> None:
    import os
    import shutil

    import pytest

    from tests import process

    keygen = shutil.which("ssh-keygen")
    if keygen is None:
        pytest.skip("OpenSSH is required for the real askpass protocol probe")
    source = tmp_path / "fixture"
    source.write_text("fake-askpass-token")
    key = tmp_path / "test-key"
    created = process.run(
        [keygen, "-q", "-t", "ed25519", "-N", "fake-askpass-token", "-f", str(key)],
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert created.returncode == 0, created.stderr
    linked = _run("__link-aliases", str(tmp_path))
    assert linked.returncode == 0, linked.stderr
    alias = tmp_path / ("clud-askpass.exe" if os.name == "nt" else "clud-askpass")
    env = {
        **os.environ,
        "SSH_ASKPASS": str(alias),
        "SSH_ASKPASS_REQUIRE": "force",
        "DISPLAY": os.environ.get("DISPLAY", ":clud-fixture"),
        "CLUD_INTEGRATION_TESTS": "1",
        "CLUD_TEST_SECRET_PROMPT_FILE": str(source),
    }
    result = process.run(
        [keygen, "-y", "-f", str(key)], capture_output=True, text=True, timeout=15, env=env
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.startswith("ssh-ed25519 ")
    assert "fake-askpass-token" not in result.stdout + result.stderr
