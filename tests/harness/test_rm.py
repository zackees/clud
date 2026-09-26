"""`rm-file` / `rm-dir` on the real Claude Code (#1340).

Agents delete through clud's `rm-file` / `rm-dir`: they trash by default
into `~/.clud/trash`, only inside the session's roots (`CLUD_RM_ROOTS`, set
by the harness as clud sets it), and clud's hook allows a command made only
of them, so Claude Code runs it with no permission prompt. An agent's own
`rm` is refused with the replacement to run instead. The `/grind` caps
narrow the roots per role: a worker deletes only under its task's
directories, the integrator anywhere in its checkout.

Script note: a step's `expect` checks the tool results of the *previous*
step, so an expectation about a command sits on the step after it.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from tests.harness.harness import Harness, RunResult

OK = {"is_error": False}
MARK = {
    "worker": "You are a /grind worker",
    "integrator": "You are the /grind integrator",
}


def _bash(command: str) -> dict[str, Any]:
    return {"tool_use": {"name": "Bash", "input": {"command": command, "description": "rm"}}}


def _after(expect: dict[str, Any], step: dict[str, Any]) -> dict[str, Any]:
    return {**step, "expect": expect}


def _main(steps: list[dict[str, Any]]) -> dict[str, Any]:
    return {"default_text": "OK", "roles": [{"name": "main", "steps": steps}]}


def _no_notes(result: RunResult) -> None:
    notes = [(r["role"], r["note"]) for r in result.requests if r.get("note")]
    assert not notes, notes


def _denials(result: RunResult) -> list[Any]:
    return [
        d
        for e in result.events
        if e.get("type") == "result"
        for d in e.get("permission_denials") or []
    ]


def _trash(h: Harness) -> list[dict[str, Any]]:
    root = h.home / ".clud" / "trash"
    if not root.is_dir():
        return []
    return [
        json.loads((entry / ".clud-rm.json").read_text(encoding="utf-8"))
        for entry in sorted(root.iterdir())
        if (entry / ".clud-rm.json").is_file()
    ]


def _trashed_origins(h: Harness) -> set[str]:
    return {Path(item["origin"]).name for entry in _trash(h) for item in entry["items"]}


def _make(h: Harness, *paths: str) -> None:
    for rel in paths:
        path = h.repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(rel, encoding="utf-8")


@pytest.mark.parametrize("skip_permissions", [False, True], ids=["prompts-on", "bypass"])
def test_rm_tools_run_without_any_prompt_and_land_in_the_trash(
    harness: Harness, skip_permissions: bool
) -> None:
    h = harness
    _make(h, "build/obj/a.o", "notes.txt")
    script = _main(
        [
            _bash("rm-dir build"),
            _after(OK, _bash("rm-file notes.txt")),
            _after(OK, {"text": "DONE"}),
        ]
    )
    result = h.run("clean up", script, skip_permissions=skip_permissions)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    assert not _denials(result), _denials(result)
    assert not (h.repo / "build").exists()
    assert not (h.repo / "notes.txt").exists()
    assert _trashed_origins(h) == {"build", "notes.txt"}
    entry = _trash(h)[0]
    assert entry["role"] == "agent", entry
    assert entry["session_id"] == h.session_id, entry


def test_an_agents_rm_is_redirected_and_rm_dir_then_succeeds(harness: Harness) -> None:
    h = harness
    _make(h, "build/out.bin")
    script = _main(
        [
            _bash("rm -rf build"),
            _after(
                {"is_error": True, "content_contains": "rm-dir build"},
                _bash("rm-dir build"),
            ),
            _after(OK, {"text": "DONE"}),
        ]
    )
    result = h.run("clean up", script, skip_permissions=False)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    assert not (h.repo / "build").exists()
    assert _trashed_origins(h) == {"build"}


def test_find_exec_rm_file_runs_without_a_prompt(harness: Harness) -> None:
    h = harness
    _make(h, "build/a.tmp", "build/sub/b.tmp", "build/keep.txt")
    script = _main(
        [
            _bash("find build -name '*.tmp' -exec rm-file {} +"),
            _after(OK, {"text": "DONE"}),
        ]
    )
    result = h.run("clean up", script, skip_permissions=False)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    assert not _denials(result), _denials(result)
    assert not (h.repo / "build/a.tmp").exists()
    assert not (h.repo / "build/sub/b.tmp").exists()
    assert (h.repo / "build/keep.txt").exists()
    assert _trashed_origins(h) == {"a.tmp", "b.tmp"}


def test_rm_file_outside_the_session_roots_is_refused(harness: Harness) -> None:
    h = harness
    outside = h.root / "outside.txt"
    outside.write_text("keep", encoding="utf-8")
    script = _main(
        [
            _bash(f"rm-file {outside}"),
            _after(
                {"is_error": True, "content_contains": "outside the allowed roots"},
                {"text": "DONE"},
            ),
        ]
    )
    result = h.run("clean up", script)
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    assert outside.read_text(encoding="utf-8") == "keep"


def test_grind_worker_and_integrator_have_their_own_roots(harness: Harness) -> None:
    h = harness
    _make(h, "src/cache/old.txt", "docs/notes.txt", "build/out.bin")
    h.write_run_facts(
        {"mode": "sequential", "tasks": [{"worktree": str(h.repo), "files": ["src/cache/mod.rs"]}]}
    )

    def agent(kind: str) -> dict[str, Any]:
        return {
            "tool_use": {
                "name": "Agent",
                "input": {
                    "subagent_type": f"grind-{kind}",
                    "description": f"rm {kind}",
                    "prompt": f"RMTEST {kind}",
                },
            }
        }

    worker = {
        "name": "worker",
        "match": MARK["worker"],
        "match_prompt": "RMTEST worker",
        "steps": [
            _bash("rm-file src/cache/old.txt"),
            _after(OK, _bash("rm-file docs/notes.txt")),
            _after(
                {"is_error": True, "content_contains": "may delete only under"},
                {"text": "WORKER DONE"},
            ),
        ],
    }
    integrator = {
        "name": "integrator",
        "match": MARK["integrator"],
        "match_prompt": "RMTEST integrator",
        "steps": [_bash("rm-dir build"), _after(OK, {"text": "INTEGRATOR DONE"})],
    }
    main = {
        "name": "main",
        "steps": [agent("worker"), agent("integrator"), {"text": "DONE"}],
    }
    script = {"default_text": "OK", "roles": [worker, integrator, main]}
    result = h.run("clean up", script, timeout=300, extra_env={"CLUD_ALLOW_GRIND_AGENTS": "1"})
    assert result.returncode == 0, result.stdout[-3000:]
    _no_notes(result)
    roles = {r.get("agent_type") for r in result.hooks if r.get("tool_name") == "Bash"}
    assert roles == {"grind-worker", "grind-integrator"}, roles
    assert not (h.repo / "src/cache/old.txt").exists()
    assert (h.repo / "docs/notes.txt").exists(), "outside the worker's task directories"
    assert not (h.repo / "build").exists()
    assert _trashed_origins(h) == {"old.txt", "build"}
