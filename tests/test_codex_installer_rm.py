"""Pinned Codex standalone installer in a clud child environment (#1461 U1-U3).

The upstream script is stored byte-for-byte as base64 in the fixture.
Only RELEASES_BASE_URL is patched at test time to a file:// tree containing a
local, checksummed package; a curl guard rejects every network URL.
"""

from __future__ import annotations

import base64
import hashlib
import io
import json
import os
import tarfile
from pathlib import Path

import pytest

from tests import process
from tests.integration._daemon_helpers import stop_daemon

SCRIPT = Path(__file__).parent / "fixtures" / "codex-install-150e3cf6.b64"
SCRIPT_SHA256 = "150e3cf675682efeaac115aa3747add3f27887896d04ce6d0b56478d8b428bf6"
VERSION = "0.157.1"
TARGET = "x86_64-unknown-linux-musl"


def _package(path: Path) -> str:
    path.parent.mkdir(parents=True)
    members = {
        "bin/codex": (f"#!/bin/sh\necho 'codex {VERSION}'\n".encode(), 0o755),
        "bin/codex-code-mode-host": (b"#!/bin/sh\nexit 0\n", 0o755),
        "codex-path/rg": (b"#!/bin/sh\nexit 0\n", 0o755),
        "codex-resources/bwrap": (b"#!/bin/sh\nexit 0\n", 0o755),
        "codex-package.json": (b"{}\n", 0o644),
    }
    with tarfile.open(path, "w:gz") as archive:
        for name, (body, mode) in members.items():
            info = tarfile.TarInfo(name)
            info.mode = mode
            info.size = len(body)
            archive.addfile(info, io.BytesIO(body))
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _fixture(tmp_path: Path) -> tuple[Path, dict[str, str], Path, Path, Path]:
    raw = base64.b64decode(SCRIPT.read_bytes())
    assert hashlib.sha256(raw).hexdigest() == SCRIPT_SHA256
    home = tmp_path / "home"
    home.mkdir()
    release_base = tmp_path / "releases"
    release_dir = release_base / "releases" / VERSION
    asset = f"codex-package-{TARGET}.tar.gz"
    archive_digest = _package(release_dir / asset)
    checksum = release_dir / "codex-package_SHA256SUMS"
    checksum.write_text(f"{archive_digest}  {asset}\n", encoding="utf-8")
    checksum_digest = hashlib.sha256(checksum.read_bytes()).hexdigest()
    metadata = {
        "tag_name": f"rust-v{VERSION}",
        "assets": [
            {"name": asset, "digest": f"sha256:{archive_digest}"},
            {"name": checksum.name, "digest": f"sha256:{checksum_digest}"},
        ],
    }
    (release_dir / "release.json").write_text(json.dumps(metadata), encoding="utf-8")

    original = 'RELEASES_BASE_URL="https://releases.openai.com/codex"'
    assert raw.count(original.encode()) == 1
    patched = raw.decode().replace(original, f'RELEASES_BASE_URL="file://{release_base}"')
    script = tmp_path / "install.sh"
    script.write_text(patched, encoding="utf-8")

    launcher = tmp_path / "launcher"
    launcher.mkdir()
    codex = launcher / "codex"
    codex.write_text(
        "#!/bin/sh\nexec sh -c 'cat \"$CODEX_INSTALL_SCRIPT\" | CODEX_NON_INTERACTIVE=1 sh'\n",
        encoding="utf-8",
    )
    codex.chmod(0o755)
    curl = launcher / "curl"
    curl.write_text(
        "#!/bin/sh\n"
        "for arg do case \"$arg\" in http://*|https://*) "
        "echo 'network disabled' >&2; exit 99;; esac; done\n"
        "exec /usr/bin/curl \"$@\"\n",
        encoding="utf-8",
    )
    curl.chmod(0o755)
    temporary = tmp_path / "tmp"
    temporary.mkdir()
    env = os.environ.copy()
    env.update(
        HOME=str(home),
        CODEX_HOME=str(home / ".codex"),
        CODEX_INSTALL_SCRIPT=str(script),
        CODEX_INSTALLER_USE_RELEASES_OPENAI_COM="1",
        CODEX_RELEASE=VERSION,
        TMPDIR=str(temporary),
        PATH=os.pathsep.join((str(launcher), "/usr/bin", "/bin")),
    )
    return home, env, temporary, launcher, script


def _run_installer(
    tmp_path: Path, env: dict[str, str], route: str
) -> process.CompletedProcess:
    binary = Path(os.environ.get("CLUD_TEST_BINARY", "/build/target/debug/clud"))
    route_args = (
        ["--no-daemon"] if route == "foreground" else ["--experimental-daemon-centralized"]
    )
    return process.run(
        [str(binary), "--codex", *route_args, "--subprocess", "-p", "update"],
        cwd=str(tmp_path),
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )


@pytest.mark.parametrize("route", ["foreground", "daemon"])
@pytest.mark.parametrize("prior", ["none", "stale", "complete"])
def test_pinned_codex_installer_in_clud_child_environment(
    tmp_path: Path, prior: str, route: str
) -> None:
    home, env, temporary, _launcher, _script = _fixture(tmp_path)
    state_dir = tmp_path / "daemon-state"
    env["CLUD_DAEMON_STATE_DIR"] = str(state_dir)
    env["CLUD_DAEMON_TEST_MODE"] = "1"
    standalone = home / ".codex" / "packages" / "standalone"
    releases = standalone / "releases"
    bin_dir = home / ".local" / "bin"
    if prior == "stale":
        releases.mkdir(parents=True)
        (releases / ".staging.old").mkdir()
        (standalone / ".current.old").symlink_to("missing")
        bin_dir.mkdir(parents=True)
        (bin_dir / ".codex.old").symlink_to("missing")
    try:
        if prior == "complete":
            first = _run_installer(tmp_path, env, route)
            assert first.returncode == 0, first.stderr

        result = _run_installer(tmp_path, env, route)
        assert result.returncode == 0, result.stderr
        target = releases / f"{VERSION}-{TARGET}"
        assert (standalone / "current").resolve() == target.resolve()
        assert (bin_dir / "codex").resolve() == (target / "bin" / "codex").resolve()
        assert not list(releases.glob(".staging.*"))
        assert not list(standalone.glob(".current.*"))
        assert not list(bin_dir.glob(".codex.*"))
        assert not list(temporary.iterdir()), "installer left an mktemp directory"
    finally:
        if route == "daemon":
            binary = Path(os.environ.get("CLUD_TEST_BINARY", "/build/target/debug/clud"))
            stop_daemon(binary, state_dir, env)
