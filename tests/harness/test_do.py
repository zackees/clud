"""`/do` typed in the real Claude Code: what the model receives (#1322).

`/do`'s body is one `` !`clud do-prompt "$ARGUMENTS"` `` line that Claude Code
runs at invocation, so the model's first request carries the rendered prompt:
the starting-branch verdict and the contract. Each scenario builds a world
(branch, PRs and issues on the fake GitHub), types `/do <target>`, and
asserts on that first request.
"""

from __future__ import annotations

import json

import pytest

from tests import process
from tests.harness.harness import Harness

ISSUE = "https://github.com/o/r/issues/7"
META = "https://github.com/o/r/issues/9"
ACK = {"default_text": "ACK", "roles": [{"name": "main", "steps": [{"text": "ACK"}]}]}


def _gh(harness: Harness, **overrides: object) -> None:
    state = {
        "repo": "o/r",
        "issues": {
            "7": {"title": "fix the thing", "body": "please", "sub_issues": []},
            "9": {
                "title": "meta",
                "body": "children",
                "sub_issues": [{"number": 10, "state": "open"}],
            },
        },
        "prs": [],
    }
    state.update(overrides)
    harness.write_gh_state(state)


def _branch(harness: Harness, name: str, *, commit: bool) -> None:
    harness.git("switch", "-q", "-c", name)
    if commit:
        (harness.repo / f"{name.replace('/', '_')}.txt").write_text("work\n", encoding="utf-8")
        harness.git("add", "-A")
        harness.git("commit", "-q", "-m", f"work on {name}")


def _do(harness: Harness, target: str, **run_kwargs: object) -> str:
    result = harness.run(f"/do {target}", ACK, **run_kwargs)  # type: ignore[arg-type]
    assert result.returncode == 0, result.stdout[-2000:]
    return result.prompt_text("main")


def _rendered(text: str) -> None:
    assert "rendered for this checkout by `clud do-prompt`" in text, text[:2000]
    assert "!`clud" not in text
    assert "$ARGUMENTS" not in text


def test_h1_default_branch_single_issue(harness: Harness) -> None:
    _gh(harness)
    text = _do(harness, ISSUE)
    _rendered(text)
    assert "Verdict: `default`" in text
    assert "Read the issue and implement it" in text


def test_h2_open_pr_branch_is_in_progress(harness: Harness) -> None:
    _branch(harness, "feat/x", commit=True)
    _gh(harness, prs=[{"number": 41, "head": "feat/x", "state": "OPEN"}])
    text = _do(harness, ISSUE)
    _rendered(text)
    assert "Verdict: `in_progress`" in text
    assert "Continue on it; `feat/x` is your starting branch" in text


def test_h3_merged_pr_branch_is_stale(harness: Harness) -> None:
    _branch(harness, "feat/x", commit=True)
    _gh(harness, prs=[{"number": 812, "head": "feat/x", "state": "MERGED"}])
    text = _do(harness, ISSUE)
    _rendered(text)
    assert "Verdict: `stale_merged`" in text
    assert "PR #812 is merged" in text
    assert "git switch main && git pull --ff-only" in text


def test_h4_empty_branch_is_stale(harness: Harness) -> None:
    _branch(harness, "feat/empty", commit=False)
    _gh(harness)
    text = _do(harness, ISSUE)
    assert "Verdict: `stale_empty`" in text


def test_h5_uncommitted_changes_stop_and_ask(harness: Harness) -> None:
    (harness.repo / "README.md").write_text("# edited\n", encoding="utf-8")
    _gh(harness)
    text = _do(harness, ISSUE)
    assert "Verdict: `dirty`" in text
    assert "Stop and ask the user" in text
    assert "README.md" in text


def test_h6_meta_issue_routes_to_grind(harness: Harness) -> None:
    _gh(harness)
    text = _do(harness, META)
    _rendered(text)
    assert f"invoke `/grind {META}`" in text
    assert "Read the issue and implement it" not in text


def test_h7_broken_gh_stops(harness: Harness) -> None:
    _gh(harness, fail=True)
    text = _do(harness, ISSUE)
    _rendered(text)
    assert "Stop. Tell the user:" in text
    assert "Read the issue and implement it" not in text


def test_h8_free_form_goal(harness: Harness) -> None:
    _gh(harness)
    text = _do(harness, "refactor the launcher")
    _rendered(text)
    assert "The goal: refactor the launcher" in text
    assert "validated, tested, pushed and merged" in text


def test_h9_renders_with_permission_prompts_on(harness: Harness) -> None:
    # No --dangerously-skip-permissions: the skill's allowed-tools must cover it.
    _gh(harness)
    text = _do(harness, ISSUE, skip_permissions=False)
    _rendered(text)
    assert "Verdict: `default`" in text


def test_h10_command_guard_and_rendering(harness: Harness) -> None:
    # With clud-cmd-scan registered, the prompt still renders. Record whether
    # the `!` command itself was seen by PreToolUse: the documented answer
    # lives in testing-tiers.md; this test pins it.
    _gh(harness)
    result = harness.run(f"/do {ISSUE}", ACK)
    _rendered(result.prompt_text("main"))
    seen = [
        h
        for h in result.hooks
        if h.get("event") == "PreToolUse" and "do-prompt" in json.dumps(h.get("tool_input"))
    ]
    assert seen == [], "the `!` expansion is not a tool call and never reaches PreToolUse"


def test_h11_missing_clud_aborts_before_any_model_call(harness: Harness) -> None:
    # With `clud` off PATH (only possible outside a clud-launched session),
    # the `!` command fails, and
    # Claude Code aborts the skill before calling the model: nothing
    # half-rendered ever reaches it. Assert the structure, not Claude Code's or
    # the shell's wording.
    harness.hide_clud = True
    _gh(harness)
    result = harness.run(f"/do {ISSUE}", ACK)
    assert result.requests == []
    errors = [
        e
        for e in result.events
        if e.get("type") == "user" and "do-prompt" in json.dumps(e) and "stderr" in json.dumps(e)
    ]
    assert errors, json.dumps(result.events)[:2000]


@pytest.mark.parametrize("target", [ISSUE, META, "refactor the launcher"])
def test_h12_clud_do_prompt_and_slash_do_agree(harness: Harness, target: str) -> None:
    _branch(harness, "feat/x", commit=True)
    _gh(harness, prs=[{"number": 812, "head": "feat/x", "state": "MERGED"}])
    direct = process.run(
        [str(harness.clud), "do-prompt", target],
        cwd=str(harness.repo),
        capture_output=True,
        text=True,
        env=harness.env(),
        timeout=60,
    )
    assert direct.returncode == 0, direct.stdout
    text = _do(harness, target)
    # Every line of the direct render reached the model through /do.
    for line in (direct.stdout or "").splitlines():
        if line.strip():
            assert json.dumps(line)[1:-1] in text, line


def test_h13_clud_do_seed_goal_do_renders_the_same_prompt(harness: Harness) -> None:
    # `clud do <url>` seeds exactly `/goal /do <url>`. /goal takes the rest of
    # the line as its condition, so `/do` is NOT expanded there: the model
    # reads "/do <url>" and loads the skill itself with the Skill tool. That
    # load must render the same prompt.
    _gh(harness)
    script = {
        "default_text": "ACK",
        "roles": [
            {
                "name": "main",
                "steps": [
                    {"tool_use": {"name": "Skill", "input": {"skill": "do", "args": ISSUE}}},
                    {"text": "ACK"},
                ],
            }
        ],
    }
    result = harness.run(f"/goal /do {ISSUE}", script)
    assert result.returncode == 0, result.stdout[-2000:]
    first = result.first_request("main")
    assert f"/do {ISSUE}" in json.dumps(first.get("messages"))
    after_skill = json.dumps([r.get("messages") for r in result.requests if r.get("turn") == 1])
    assert "rendered for this checkout by `clud do-prompt`" in after_skill, after_skill[-3000:]
    assert "Verdict: `default`" in after_skill
