"""Guard the /grind plan-then-ask contract in the bundled assets (#1407, #1392 §0 and §2).

The harness tests in `tests/harness/test_grind_upfront.py` drive a scripted
router; these check that the router skill the real model reads states the
same contract.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

ASSETS = Path(__file__).resolve().parents[1] / "crates/clud-bin/assets"
ROUTER = ASSETS / "skills/grind/SKILL.md"
AGENTS = ASSETS / "agents"


def _router() -> str:
    return ROUTER.read_text(encoding="utf-8")


def _section(text: str, heading: str) -> str:
    start = text.index(f"\n## {heading}")
    end = text.find("\n## ", start + 1)
    return text[start : end if end != -1 else len(text)]


def test_router_orders_routing_plan_preflight_questions_prework() -> None:
    text = _router()
    order = [
        "## 1. Intake",
        "## 1b. Plan before asking",
        "## 1c. Repo-state preflight",
        "## 2. One question round",
        "## 3. Record the run",
        "## 3b. Assemble the plan",
        "## 4. Run",
        "## 5. Finish",
    ]
    positions = [text.index(h) for h in order]
    assert positions == sorted(positions), list(zip(order, positions, strict=True))


def test_question_round_fits_two_ask_user_question_calls() -> None:
    round_ = _section(_router(), "2. One question round")
    assert "At most 2 AskUserQuestion calls in total, and nothing is asked twice." in round_
    assert "at most 4 questions per call" in round_
    call1 = re.search(r"\*\*Call 1[^*]*:\*\* (.*?)\.\n", round_)
    call2 = re.search(r"\*\*Call 2[^*]*:\*\* (.*?)\.\n", round_, re.DOTALL)
    assert call1 and call2, round_
    items = [i.strip() for i in call1.group(1).split(",")]
    items += [" ".join(i.split()) for i in call2.group(1).split(",")]
    # U12: every item, at most 4 per call.
    assert items == [
        "Dirty repo",
        "Regroup",
        "Mode",
        "Models",
        "Local CI",
        "Scripts",
        "Feature merge policy",
        "Problem reporting",
    ], items
    for item in items:
        assert f"- **{item}" in round_, item


def test_carry_is_offered_only_with_a_feature_stage() -> None:
    text = _router()
    # U3 / U4.
    assert "Carry into the grind worktree (only when\n  the plan has a feature stage)" in text
    assert "offered only when the plan has a\n  feature stage" in text


def test_merge_policy_is_asked_only_with_a_feature_stage() -> None:
    round_ = _section(_router(), "2. One question round")
    # U13.
    assert "**Feature merge policy** (asked once, only when the plan has feature" in round_
    assert "**Problem reporting** (always)" in round_


def test_preflight_never_touches_the_runs_own_files() -> None:
    preflight = _section(_router(), "1c. Repo-state preflight")
    exclude = "':(exclude).clud/grind'"
    assert f"git status --porcelain -uall -- . {exclude}" in preflight
    assert f"git stash push -u -m grind-<run-id> -- . {exclude}" in preflight
    assert f"git stash push -u -m grind-<run-id>-carry -- . {exclude}" in preflight
    assert "git add -A -- .\n  ':(exclude).clud/grind'" in preflight
    assert "Never push it." in preflight
    assert "remove the\n  plan-phase `.clud/grind/run.json`" in preflight


def test_run_json_records_every_up_front_answer() -> None:
    record = _section(_router(), "3. Record the run")
    block = re.search(r"```json\n(.*?)\n```", record, re.DOTALL)
    assert block, record
    example = json.loads(block.group(1))
    # U14.
    for key in ("mode", "preflight", "feature_merge", "problem_reporting", "tracks", "meta"):
        assert key in example, key
    assert set(example["preflight"]) >= {"action", "branch"}
    for action in ("`stash`", "`wip`", "`carry`", "`none`"):
        assert action in record, action


def test_finish_restores_only_what_preflight_recorded() -> None:
    finish = _section(_router(), "5. Finish (always, as the very last step)")
    assert "**Restore only what `preflight` recorded.**" in finish
    for action in ("stash", "wip", "carry", "none"):
        assert f"   - `{action}`:" in finish, action
    assert "never a bare pop" in finish
    assert "never ask about\n   repo state here" in finish


def test_no_question_after_prework() -> None:
    text = _router()
    # U10: the router's own rule; the hook cannot tell the main session apart.
    assert "From the moment prework starts" in text
    assert "nobody asks: no agent and not the main session calls" in text


def test_no_grind_agent_is_offered_ask_user_question() -> None:
    # U11, first layer: the agents' tool lists leave AskUserQuestion out; the
    # hook's denial (block_bad_cmd_grind_caps::tool_reason) is the second.
    agents = sorted(AGENTS.glob("grind-*.md"))
    assert agents
    for agent in agents:
        tools = next(
            line
            for line in agent.read_text(encoding="utf-8").splitlines()
            if line.startswith("tools:")
        )
        assert "AskUserQuestion" not in tools, agent.name
