"""Process tests for the session `gh` read broker's shim half (#1743).

The daemon side (TTL, single-flight, 304 revalidation, ledger) is covered by
the Rust tests in `crates/clud-bin/src/gh_broker/service/tests.rs`. These
tests drive the real `gh` alias against a fake daemon listener and a fake
`gh`, and pin the output contract: a brokered read prints exactly what the
real `gh` prints, and every miss runs the real `gh` with the caller's argv.
"""

from __future__ import annotations

import base64
import http.server
import json
import os
import shutil
import sys
import threading
from pathlib import Path

import pytest

from tests import process
from tests.shim_env import session_env

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="POSIX recording fixtures")

# Bytes a naive text round trip would corrupt: CRLF, non-UTF-8, no final LF.
BODY = b'{"id":1,"name":"caf\\u00e9"}\r\n\xff tail-without-newline'
TOKEN = "test-capability"

FAKE_GH = """#!{python}
import sys, urllib.request
args = sys.argv[1:]
with open({log!r}, "a") as log:
    log.write(" ".join(args) + "\\n")
url = next((a for a in args if a.startswith("http://")), None)
if url:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({{}}))
    with opener.open(url) as response:
        body = response.read()
    with open({log!r} + ".replayed", "wb") as replayed:
        replayed.write(body)
    sys.stdout.buffer.write(body)
    sys.exit(0)
sys.stdout.buffer.write({body!r})
sys.exit(0)
"""


def _binary(name: str) -> Path:
    clud = os.environ.get("CLUD_TEST_BINARY")
    candidate = (
        Path(clud).with_name(name)
        if clud
        else Path(__file__).resolve().parents[1] / "target" / "debug" / name
    )
    assert candidate.is_file(), candidate
    return candidate


class FakeDaemon:
    """The daemon's `/gh/read` and `/gh/invalidate` routes, recorded."""

    def __init__(self, state_dir: Path, status: int = 200) -> None:
        self.reads: list[dict] = []
        self.invalidations = 0
        self.invalidate_bodies: list[dict] = []
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(length) or b"{}")
                if f"clud_dashboard_token={TOKEN}" not in self.headers.get("Cookie", ""):
                    self.send_response(403)
                    self.end_headers()
                    return
                if self.path == "/gh/invalidate":
                    outer.invalidations += 1
                    outer.invalidate_bodies.append(payload)
                    reply = b"{}"
                    code = 200
                else:
                    outer.reads.append(payload)
                    code = status
                    reply = json.dumps(
                        {
                            "status": 200,
                            "headers": [["Content-Type", "application/json"]],
                            "body_b64": base64.b64encode(BODY).decode(),
                            "outcome": "ok",
                        }
                    ).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(reply)))
                self.end_headers()
                self.wfile.write(reply)

            def log_message(self, *_args: object) -> None:
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        port = self.server.server_address[1]
        state_dir.mkdir(parents=True, exist_ok=True)
        (state_dir / "daemon.json").write_text(
            json.dumps({"dashboard_port": port, "dashboard_token": TOKEN}),
            encoding="utf-8",
        )
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


class World:
    def __init__(self, tmp_path: Path) -> None:
        shim_dir = tmp_path / "shim"
        shim_dir.mkdir()
        self.alias = shim_dir / "gh"
        shutil.copy2(_binary("clud-shim"), self.alias)
        self.log = tmp_path / "gh-calls"
        self.gh = tmp_path / "real-gh"
        self.gh.write_text(
            FAKE_GH.format(python=sys.executable, log=str(self.log), body=BODY),
            encoding="utf-8",
        )
        self.gh.chmod(0o755)
        self.state = tmp_path / "state"
        self.env = os.environ.copy() | session_env(_binary("clud-shim"), shim_dir)
        self.env.update(
            CLUD_GH_SHIM_TARGET=str(self.gh),
            CLUD_GH_READ_BROKER="1",
            CLUD_DAEMON_STATE_DIR=str(self.state),
            CLUD_SESSION_ID="session-1743",
            GH_TOKEN="forwarded-token",
        )
        self.cwd = tmp_path

    def run(self, *args: str) -> process.CompletedProcess:
        return process.run(
            [str(self.alias), *args],
            env=self.env,
            cwd=self.cwd,
            capture_output=True,
            timeout=30,
        )

    def calls(self) -> list[str]:
        if not self.log.exists():
            return []
        return self.log.read_text(encoding="utf-8").splitlines()


def test_brokered_read_prints_exactly_what_the_real_gh_prints(tmp_path: Path) -> None:
    world = World(tmp_path)
    daemon = FakeDaemon(world.state)
    try:
        direct = process.run(
            [str(world.gh), "api", "repos/o/r/actions/runs/1", "--jq", ".name"],
            capture_output=True,
            timeout=30,
        )
        world.log.unlink()
        result = world.run("api", "repos/o/r/actions/runs/1", "--jq", ".name")
    finally:
        daemon.close()
    assert result.returncode == 0, result
    assert result.stdout == direct.stdout
    # The bytes the real gh formatted are the upstream body, byte for byte.
    assert Path(str(world.log) + ".replayed").read_bytes() == BODY
    # The real gh formatted the brokered body: same argv, endpoint swapped
    # for the one-shot loopback URL, formatting flags kept.
    (call,) = world.calls()
    words = call.split(" ")
    assert words[0] == "api"
    assert words[1].startswith("http://127.0.0.1:")
    assert "/clud-gh-replay/" in words[1]
    assert words[2:] == ["--jq", ".name"]
    (read,) = daemon.reads
    assert read["endpoint"] == "repos/o/r/actions/runs/1"
    assert read["gh"] == str(world.gh)
    assert read["session_id"] == "session-1743"
    assert ["GH_TOKEN", "forwarded-token"] in read["env"]
    assert daemon.invalidations == 0


def test_no_daemon_runs_the_real_gh_unchanged(tmp_path: Path) -> None:
    world = World(tmp_path)
    result = world.run("api", "repos/o/r")
    assert result.returncode == 0, result
    assert result.stdout.startswith(b'{"id":1')
    assert world.calls() == ["api repos/o/r"]


def test_a_daemon_miss_runs_the_real_gh_unchanged(tmp_path: Path) -> None:
    world = World(tmp_path)
    daemon = FakeDaemon(world.state, status=409)
    try:
        result = world.run("api", "repos/o/r/pulls/9")
    finally:
        daemon.close()
    assert result.returncode == 0, result
    assert result.stdout.startswith(b'{"id":1')
    assert world.calls() == ["api repos/o/r/pulls/9"]
    assert len(daemon.reads) == 1


def test_writes_pass_through_and_invalidate(tmp_path: Path) -> None:
    world = World(tmp_path)
    daemon = FakeDaemon(world.state)
    try:
        result = world.run("api", "-X", "POST", "repos/o/r/issues", "-f", "title=x")
        unbrokered = world.run("api", "repos/o/r", "--paginate")
    finally:
        daemon.close()
    assert result.returncode == 0, result
    assert unbrokered.returncode == 0, unbrokered
    assert world.calls() == [
        "api -X POST repos/o/r/issues -f title=x",
        "api repos/o/r --paginate",
    ]
    assert daemon.reads == []
    assert daemon.invalidations == 1
    # `repos/o/r/issues` names no issue: every cached read is stale.
    assert daemon.invalidate_bodies == [{}]


def test_recognized_writes_invalidate_only_what_they_name(tmp_path: Path) -> None:
    world = World(tmp_path)
    world.env.pop("GH_REPO", None)
    daemon = FakeDaemon(world.state)
    try:
        world.run("pr", "merge", "7", "-R", "o/r", "--squash")
        world.run("run", "rerun", "99")
        world.run("pr", "create", "--fill")
    finally:
        daemon.close()
    assert world.calls() == [
        "pr merge 7 -R o/r --squash",
        "run rerun 99",
        "pr create --fill",
    ]
    assert daemon.invalidate_bodies == [
        {"tags": ["o/r#num:7", "*#num:7", "o/r#runs", "*#runs", "other"]},
        {"tags": ["run:99", "*#runs", "*#checks", "other"]},
        {},
    ]


def test_disabled_broker_never_contacts_the_daemon(tmp_path: Path) -> None:
    world = World(tmp_path)
    world.env["CLUD_GH_READ_BROKER"] = "0"
    daemon = FakeDaemon(world.state)
    try:
        result = world.run("api", "repos/o/r")
        world.run("pr", "merge", "1")
    finally:
        daemon.close()
    assert result.stdout.startswith(b'{"id":1')
    assert world.calls() == ["api repos/o/r", "pr merge 1"]
    assert daemon.reads == []
    assert daemon.invalidations == 0
