"""Real Claude Code on a scripted mock agent backend (#1323).

A :class:`Harness` builds an isolated world in a temp directory and runs the
installed Claude Code against ``mock-agent serve``:

* **home** — ``HOME`` and ``CLAUDE_CONFIG_DIR`` (``home/.claude``), populated
  by ``clud install-assets`` so the test runs the skills, agents and workflow
  users get.
* **repo** — a git checkout on ``main`` with a local bare ``origin``.
* **bin** — first on PATH: a fake ``gh`` (``fake_gh.py``) and an ``rm`` that is
  a copy of the built ``clud-shim`` (``clud-cmd-scan``'s rm-identity check
  requires the first ``rm`` on PATH to be byte-identical to it).
* **logs** — the backend's request log, the hook recorder's log and, with
  ``run(answers=...)``, the answered-questions log.

AskUserQuestion: `claude -p` only offers the tool when a permission prompt
tool is configured, so ``run(answers={...})`` adds ``--mcp-config`` +
``--permission-prompt-tool`` for the stub server ``answer_mcp.py`` and
registers the PreToolUse hook ``answer_hook.py``, which answers each call via
``updatedInput.answers``. A test that scripts AskUserQuestion must pass
``answers`` (``{"*": label}`` for a default); see :meth:`Harness.answer_args`.

PATH never contains clud's own shim directories (``~/.clud/state/shims``,
``~/.clud/state/rm-shim``): clud's ``python`` shim refuses to run outside a
clud session, so a hook that says ``python`` would die. Scripts and hooks here
run under the test's own interpreter.

Opt-in: these tests need an installed Claude Code and run only with
``CLUD_REAL_CLAUDE_TESTS=1``. See ``docs/architecture/testing-tiers.md``.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import time
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from tests import process

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
EXE = ".exe" if os.name == "nt" else ""
ENABLED = os.environ.get("CLUD_REAL_CLAUDE_TESTS") == "1"
# The AskUserQuestion answerer's MCP server and its permission-prompt tool
# (``answer_mcp.py``; the names must match its SERVER and TOOL).
ANSWER_SERVER = "clud_harness"
ANSWER_TOOL = f"mcp__{ANSWER_SERVER}__answer"


def _built(name: str, env_var: str) -> Path:
    override = os.environ.get(env_var)
    if override:
        return Path(override)
    return ROOT / "target" / "debug" / f"{name}{EXE}"


def claude_executable() -> str | None:
    override = os.environ.get("CLUD_REAL_CLAUDE")
    if override:
        return override
    return shutil.which("claude")


@dataclass
class RunResult:
    returncode: int
    stdout: str
    requests: list[dict[str, Any]]
    hooks: list[dict[str, Any]]
    events: list[dict[str, Any]] = field(default_factory=list)
    # One record per AskUserQuestion question the harness answered:
    # ``{"t": ms, "agent_type", "question", "header", "answer"}``.
    questions: list[dict[str, Any]] = field(default_factory=list)

    def first_request(self, role: str = "main") -> dict[str, Any]:
        for request in self.requests:
            if request.get("role") == role:
                return request
        raise AssertionError(
            f"no request from role {role!r}; roles seen: {[r.get('role') for r in self.requests]}"
        )

    def prompt_text(self, role: str = "main") -> str:
        """Everything the model saw from the user in the role's first request."""
        return json.dumps(self.first_request(role).get("messages"))

    def questions_before(self, role: str = "grind-prework") -> list[dict[str, Any]]:
        """See :func:`questions_before`."""
        return questions_before(self, role)

    def questions_after(self, role: str = "grind-prework") -> list[dict[str, Any]]:
        """See :func:`questions_after`."""
        return questions_after(self, role)


def _split_at_role(
    result: RunResult, role: str
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """AskUserQuestion hook records before / from `role`'s first hook event."""
    before: list[dict[str, Any]] = []
    after: list[dict[str, Any]] = []
    seen = False
    for record in result.hooks:
        seen = seen or record.get("agent_type") == role
        if record.get("tool_name") == "AskUserQuestion":
            (after if seen else before).append(record)
    return before, after


def questions_before(result: RunResult, role: str = "grind-prework") -> list[dict[str, Any]]:
    """The `AskUserQuestion` hook records made before `role` first acted.

    ``hooks.jsonl`` is appended in call order, so this walks it and stops at
    the first record whose ``agent_type`` is `role` (for the up-front rule,
    ``grind-prework``); if `role` never appears, every AskUserQuestion record
    is returned. Pair it with :func:`questions_after` to assert the whole
    question round happened before prework and none came after (#1392 U10).
    """
    return _split_at_role(result, role)[0]


def questions_after(result: RunResult, role: str = "grind-prework") -> list[dict[str, Any]]:
    """The `AskUserQuestion` hook records from `role`'s first event on, made
    by any agent including the main session; empty if `role` never ran."""
    return _split_at_role(result, role)[1]


class Harness:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.home = root / "home"
        self.config = self.home / ".claude"
        self.repo = root / "repo"
        self.origin = root / "origin.git"
        self.bin = root / "bin"
        self.logs = root / "logs"
        for directory in (self.config, self.bin, self.logs):
            directory.mkdir(parents=True, exist_ok=True)
        self.clud = _built("clud", "CLUD_HARNESS_CLUD")
        self.mock_agent = _built("mock-agent", "CLUD_HARNESS_MOCK_AGENT")
        claude = claude_executable()
        if claude is None:
            raise RuntimeError("Claude Code is not installed; set CLUD_REAL_CLAUDE")
        self.claude = claude
        for binary in (self.clud, self.mock_agent):
            if not binary.is_file():
                raise RuntimeError(
                    f"{binary} is missing; build it first (`bash build` or soldr cargo build)"
                )
        self.hide_clud = False
        # Each `run` is one Claude Code session, launched with `--session-id`
        # so a test knows the id up front: the `/grind` run facts live at
        # `~/.clud/tmp/grind/<session_id>.json` (#1337). Claude Code refuses
        # to reuse an id, so a second run needs `new_session()` first.
        self.session_id = str(uuid.uuid4())
        self._session_used = False
        self.gh_state = root / "gh-state.json"
        self.write_gh_state({"repo": "o/r", "issues": {}, "prs": []})
        self._install_bin()

    # ---- world -----------------------------------------------------------------

    def _install_bin(self) -> None:
        gh = self.bin / "gh"
        gh.write_text(
            f'#!/bin/sh\nexec "{sys.executable}" "{HERE / "fake_gh.py"}" "$@"\n', encoding="utf-8"
        )
        gh.chmod(0o755)
        shim = self.clud.parent / f"clud-shim{EXE}"
        shutil.copyfile(shim, self.bin / f"rm{EXE}")
        (self.bin / f"rm{EXE}").chmod(0o755)

    def write_gh_state(self, state: dict[str, Any]) -> None:
        self.gh_state.write_text(json.dumps(state, indent=1), encoding="utf-8")

    def read_gh_state(self) -> dict[str, Any]:
        return json.loads(self.gh_state.read_text(encoding="utf-8"))

    def git(self, *args: str, cwd: Path | None = None) -> str:
        result = process.run(
            ["git", *args],
            cwd=str(cwd or self.repo),
            capture_output=True,
            text=True,
            env=self.env(),
            timeout=60,
        )
        if result.returncode != 0:
            raise RuntimeError(f"git {' '.join(args)} failed: {result.stdout}{result.stderr}")
        return (result.stdout or "").strip()

    def make_repo(self) -> None:
        """`repo` on `main` with one commit, pushed to a bare `origin`."""
        self.repo.mkdir(parents=True, exist_ok=True)
        self.git("init", "-q", "--bare", "-b", "main", str(self.origin), cwd=self.root)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "harness@example.invalid")
        self.git("config", "user.name", "clud harness")
        (self.repo / "README.md").write_text("# harness repo\n", encoding="utf-8")
        self.git("add", "README.md")
        self.git("commit", "-q", "-m", "initial")
        self.git("remote", "add", "origin", str(self.origin))
        self.git("push", "-q", "-u", "origin", "main")
        self.git("remote", "set-head", "origin", "main")

    def install_assets(self) -> None:
        result = process.run(
            [str(self.clud), "install-assets", "--home", str(self.home)],
            capture_output=True,
            text=True,
            env=self.env(),
            timeout=60,
        )
        if result.returncode != 0:
            raise RuntimeError(f"clud install-assets failed: {result.stdout}{result.stderr}")

    # ---- /grind run facts ------------------------------------------------------

    def new_session(self) -> str:
        """Start a fresh session id for the next `run`; returns it."""
        self.session_id = str(uuid.uuid4())
        self._session_used = False
        return self.session_id

    def run_facts_path(self) -> Path:
        """The `/grind` run-facts file of the current session (#1337)."""
        return self.home / ".clud" / "tmp" / "grind" / f"{self.session_id}.json"

    def write_run_facts(self, facts: dict[str, Any]) -> Path:
        """Record run facts the way the router does, for the current session."""
        path = self.run_facts_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(facts, indent=1), encoding="utf-8")
        return path

    # ---- environment -----------------------------------------------------------

    def path(self) -> str:
        """PATH for the harness: ours first, clud's session shims removed."""
        keep = [
            str(self.bin),
            str(self.clud.parent),
            str(Path(self.claude).parent),
            str(Path(sys.executable).parent),
        ]
        for entry in os.environ.get("PATH", "").split(os.pathsep):
            if (
                not entry
                or ".clud" + os.sep + "state" in entry
                or "uv" + os.sep + "archive-v0" in entry
            ):
                continue
            if entry not in keep:
                keep.append(entry)
        if self.hide_clud:
            # Simulate "clud is not installed": drop every directory that
            # holds a `clud`, including the build dir. Hooks and fixture
            # scripts use absolute paths, so they keep working.
            keep = [d for d in keep if not (Path(d) / f"clud{EXE}").exists()]
        return os.pathsep.join(keep)

    def env(self, extra: dict[str, str] | None = None) -> dict[str, str]:
        env = {
            name: os.environ[name]
            for name in ("LANG", "LC_ALL", "TERM", "SYSTEMROOT", "TEMP", "TMP")
            if name in os.environ
        }
        env.update(
            {
                "PATH": self.path(),
                "HOME": str(self.home),
                "USERPROFILE": str(self.home),
                "CLAUDE_CONFIG_DIR": str(self.config),
                "ANTHROPIC_API_KEY": "fixture-api-key-canary",
                "NO_PROXY": "127.0.0.1,localhost",
                "DISABLE_TELEMETRY": "1",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                "FAKE_GH_STATE": str(self.gh_state),
                "CLUD_EXE": str(self.clud),
                "GIT_TERMINAL_PROMPT": "0",
            }
        )
        env.update(extra or {})
        return env

    def settings(self, *, cmd_scan: bool = True, answers: dict[str, Any] | None = None) -> Path:
        """A `--settings` file: the recorder, then clud's real command guard.

        Project `.claude/settings.json` hooks do not fire under an isolated
        CLAUDE_CONFIG_DIR, so hooks are passed this way (#1323, pitfall 2).
        With `answers`, an `AskUserQuestion` hook (``answer_hook.py``) picks
        each question's option from that table and logs it with its time to
        ``logs/questions.jsonl`` (#1402); see :meth:`answer_args`.
        """
        record = f'"{sys.executable}" "{HERE / "record_hook.py"}" "{self.logs / "hooks.jsonl"}"'
        pre = [{"type": "command", "command": record}]
        if cmd_scan:
            pre.append(
                {"type": "command", "command": f'"{self.clud.parent / ("clud-cmd-scan" + EXE)}"'}
            )
        pre_entries: list[dict[str, Any]] = []
        if answers is not None:
            answers_path = self.root / "answers.json"
            answers_path.write_text(json.dumps(answers, indent=1), encoding="utf-8")
            answer = (
                f'"{sys.executable}" "{HERE / "answer_hook.py"}" '
                f'"{answers_path}" "{self.logs / "questions.jsonl"}"'
            )
            pre_entries.append(
                {"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": answer}]}
            )
        pre_entries.append({"matcher": "*", "hooks": pre})
        settings = {
            "hooks": {
                "PreToolUse": pre_entries,
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": record}]}],
            }
        }
        path = self.root / "settings.json"
        path.write_text(json.dumps(settings, indent=1), encoding="utf-8")
        return path

    def answer_args(self) -> list[str]:
        """Claude Code flags that make `AskUserQuestion` answerable in `-p`.

        Print mode drops AskUserQuestion from the model's tool list unless a
        permission prompt tool is configured (Claude Code 2.1.x: the tool's
        ``isEnabled`` is false in a non-interactive session without one), so
        the answer hook alone never fires. This registers ``answer_mcp.py`` as
        an MCP server and names its tool with ``--permission-prompt-tool``;
        the hook then answers through ``updatedInput``, and the MCP tool
        answers from the same table if Claude Code routes the call to it.
        Call :meth:`settings` with the answers first (it writes the table).
        """
        answers_path = self.root / "answers.json"
        config = {
            "mcpServers": {
                ANSWER_SERVER: {
                    "type": "stdio",
                    "command": sys.executable,
                    "args": [
                        str(HERE / "answer_mcp.py"),
                        str(answers_path),
                        str(self.logs / "questions.jsonl"),
                    ],
                }
            }
        }
        path = self.root / "answer-mcp.json"
        path.write_text(json.dumps(config, indent=1), encoding="utf-8")
        # `--mcp-config` is variadic: keep an option right after its value.
        return ["--mcp-config", str(path), "--permission-prompt-tool", ANSWER_TOOL]

    # ---- running ---------------------------------------------------------------

    @contextmanager
    def backend(self, script: dict[str, Any]) -> Iterator[str]:
        script_path = self.root / "script.json"
        script_path.write_text(json.dumps(script, indent=1), encoding="utf-8")
        port_file = self.root / "backend.port"
        port_file.unlink(missing_ok=True)
        log = self.logs / "requests.jsonl"
        log.write_text("", encoding="utf-8")
        child = process.Popen(
            [
                str(self.mock_agent),
                "serve",
                "--script",
                str(script_path),
                "--port",
                "0",
                "--log",
                str(log),
                "--port-file",
                str(port_file),
            ],
            stdout=process.PIPE,
            stderr=process.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 10
            while not port_file.is_file() or not port_file.read_text().strip():
                if child.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("mock-agent serve did not start")
                time.sleep(0.05)
            yield f"http://127.0.0.1:{port_file.read_text().strip()}"
        finally:
            child.kill()
            child.wait(timeout=10)

    def run(
        self,
        prompt: str,
        script: dict[str, Any],
        *,
        skip_permissions: bool = True,
        cmd_scan: bool = True,
        extra_args: list[str] | None = None,
        extra_env: dict[str, str] | None = None,
        timeout: float = 120,
        answers: dict[str, Any] | None = None,
    ) -> RunResult:
        if self._session_used:
            raise RuntimeError(
                f"session {self.session_id} already ran; call new_session() before another run"
            )
        self._session_used = True
        (self.logs / "hooks.jsonl").write_text("", encoding="utf-8")
        (self.logs / "questions.jsonl").write_text("", encoding="utf-8")
        with self.backend(script) as base_url:
            argv = [
                self.claude,
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--model",
                "claude-sonnet-5",
                "--settings",
                str(self.settings(cmd_scan=cmd_scan, answers=answers)),
                "--session-id",
                self.session_id,
            ]
            if answers is not None:
                argv += self.answer_args()
            if skip_permissions:
                argv.append("--dangerously-skip-permissions")
            argv += list(extra_args or [])
            argv.append(prompt)
            env = self.env({"ANTHROPIC_BASE_URL": base_url, **(extra_env or {})})
            cwd = self.repo if self.repo.is_dir() else self.root
            result = process.run(
                argv, cwd=str(cwd), capture_output=True, text=True, env=env, timeout=timeout
            )
        return RunResult(
            returncode=result.returncode,
            stdout=result.stdout or "",
            requests=_jsonl(self.logs / "requests.jsonl"),
            hooks=_jsonl(self.logs / "hooks.jsonl"),
            questions=_jsonl(self.logs / "questions.jsonl"),
            events=[
                e for e in (_json_or_none(line) for line in (result.stdout or "").splitlines()) if e
            ],
        )


def _json_or_none(line: str) -> dict[str, Any] | None:
    try:
        value = json.loads(line)
    except ValueError:
        return None
    return value if isinstance(value, dict) else None


def _jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    return [
        v
        for v in (_json_or_none(line) for line in path.read_text(encoding="utf-8").splitlines())
        if v
    ]
