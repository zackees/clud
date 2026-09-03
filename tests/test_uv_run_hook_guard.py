"""Unit coverage for the bundled `uv_run_hook_guard.py` tool.

The guard warns when an agent hook runs bare `uv run` somewhere that a
re-sync means a native build. It had no tests; #972 is why it needed them.

In #972 a Stop hook installed in a pure-Python parent repo bound itself to a
Rust-backed dependent project under `.extern-repos/` and spent ~600s and
~400s compiling it, the second fire dying with an opaque build-backend error
that named no hook at all. The guard existed for exactly that failure and
stayed silent, for two independent reasons -- it did not scan `Stop`, and its
gate required `Cargo.toml` in the scanned root, which a pure-Python parent
does not have.

Both halves are asserted here against the wrapper from the issue verbatim,
because either one alone leaves the guard silent on the incident it is named
for.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

TOOL_DIR = (
    Path(__file__).resolve().parents[1]
    / "crates"
    / "clud-bin"
    / "assets"
    / "tools"
    / "hooks"
)


def _load_guard():
    """Import the bundled tool from the assets tree.

    It is an asset, not a package module, so it is reached by path -- and by
    the real installed file rather than a copy, which could drift. The import
    sits inside a function so the module's import block stays sorted.
    """
    if str(TOOL_DIR) not in sys.path:
        sys.path.insert(0, str(TOOL_DIR))
    import uv_run_hook_guard

    return uv_run_hook_guard


guard = _load_guard()


# The wrapper from #972, in the shape that caused the incident: it resolves
# its project root by walking up from `$PWD`, so it binds to whichever
# project contains the shell rather than the one the hook was installed in.
PWD_WALK_WRAPPER = (
    'S=ci/hooks/check-on-stop.py; d=$PWD; '
    'while [ "$d" != / ] && { [ ! -f "$d/pyproject.toml" ] || [ ! -f "$d/$S" ]; }; '
    'do d=$(dirname "$d"); done; '
    'if [ -f "$d/$S" ]; then cd "$d" && uv run python "$S"; fi'
)


def _hook_config(event: str, command: str) -> str:
    return json.dumps(
        {"hooks": {event: [{"hooks": [{"type": "command", "command": command}]}]}}
    )


def _parent_repo(root: Path, event: str = "Stop", command: str = PWD_WALK_WRAPPER) -> Path:
    """A pure-Python parent repo carrying one hook. No Cargo.toml, like FastLED."""
    repo = root / "parent"
    (repo / ".claude").mkdir(parents=True)
    (repo / "pyproject.toml").write_text("[project]\nname = 'parent'\n", encoding="utf-8")
    (repo / ".claude" / "settings.json").write_text(
        _hook_config(event, command), encoding="utf-8"
    )
    return repo


def _add_extern_rust_checkout(
    repo: Path, name: str = "fbuild", *, legacy: bool = True
) -> Path:
    """The dependent project the `clud-extern-repos` convention checks out.

    `legacy` picks the pre-#986 in-tree location; the current convention puts
    it beside the repo at `<repo>-extern/`. Both are live during the migration
    and both must arm the guard.
    """
    parent = repo / ".extern-repos" if legacy else repo.parent / f"{repo.name}-extern"
    dep = parent / name
    dep.mkdir(parents=True)
    (dep / "Cargo.toml").write_text("[package]\nname = 'dep'\n", encoding="utf-8")
    (dep / "pyproject.toml").write_text(
        '[build-system]\nbuild-backend = "soldr"\n', encoding="utf-8"
    )
    return dep


def test_a_pure_python_repo_alone_is_not_scanned(tmp_path: Path) -> None:
    """The gate must stay shut without a native build to trigger.

    A bare `uv run` in a pure-Python project is a venv re-sync, not a
    compile. Warning there would be noise, and noise is what makes the real
    warning ignorable."""
    repo = _parent_repo(tmp_path)

    assert guard._repo_qualifies(repo) is False
    assert guard.scan(repo) == []


def test_a_rust_backed_extern_checkout_makes_the_parent_qualify(tmp_path: Path) -> None:
    """#972: the native build a hook triggers need not be *this* repo's.

    This is the half that kept the guard silent even once `Stop` was in
    scope -- the parent is pure Python, so the old root-only gate refused to
    look at it at all."""
    repo = _parent_repo(tmp_path)
    assert guard.scan(repo) == [], "precondition: silent before the checkout exists"

    _add_extern_rust_checkout(repo)

    assert guard._repo_qualifies(repo) is True
    offenders = guard.scan(repo)
    assert len(offenders) == 1, offenders
    assert offenders[0].event == "Stop"
    assert "uv run" in offenders[0].command


def test_a_pure_python_extern_checkout_does_not_arm_the_guard(tmp_path: Path) -> None:
    """Only a checkout whose `uv run` is a *build* counts.

    Without this the gate degrades into "has any .extern-repos/", which
    would fire on every cross-repo session regardless of cost."""
    repo = _parent_repo(tmp_path)
    dep = repo / ".extern-repos" / "plain"
    dep.mkdir(parents=True)
    (dep / "pyproject.toml").write_text("[project]\nname = 'plain'\n", encoding="utf-8")

    assert guard._repo_qualifies(repo) is False
    assert guard.scan(repo) == []


def test_stop_hooks_are_scanned(tmp_path: Path) -> None:
    """The other half of #972's silence.

    v0 excluded `Stop` reasoning that it does not pay the per-tool-call
    cost. Per-fire cost is what dominates: the issue measured 600s and 400s
    for two fires."""
    assert "Stop" in guard.SCANNED_EVENTS

    repo = _parent_repo(tmp_path, event="Stop", command="uv run python ci/lint.py")
    _add_extern_rust_checkout(repo)

    offenders = guard.scan(repo)
    assert [o.event for o in offenders] == ["Stop"], offenders


def test_session_start_is_still_out_of_scope(tmp_path: Path) -> None:
    """Deliberately narrow, so the previous test is not just "everything".

    SessionStart fires once, before the agent works; a sync there is startup
    cost the user is already paying."""
    assert "SessionStart" not in guard.SCANNED_EVENTS

    repo = _parent_repo(tmp_path, event="SessionStart", command="uv run python ci/lint.py")
    _add_extern_rust_checkout(repo)

    assert guard.scan(repo) == []


@pytest.mark.parametrize("flag", ["--no-project", "--no-sync", "--frozen"])
def test_a_guarded_uv_run_is_not_an_offender(tmp_path: Path, flag: str) -> None:
    """The remedy the warning names must actually silence it."""
    repo = _parent_repo(tmp_path, command=f"uv run {flag} python ci/lint.py")
    _add_extern_rust_checkout(repo)

    assert guard.scan(repo) == []


def test_the_current_sibling_layout_also_arms_the_guard(tmp_path: Path) -> None:
    """`<repo>-extern/` is where new checkouts go (#986).

    Covering only `.extern-repos/` would have fixed the guard for the layout
    the project is moving *away* from -- coverage that reads as present and
    is not, which is the failure mode this whole issue is about.
    """
    repo = _parent_repo(tmp_path)
    _add_extern_rust_checkout(repo, legacy=False)

    assert guard._repo_qualifies(repo) is True
    offenders = guard.scan(repo)
    assert len(offenders) == 1, offenders
    assert offenders[0].event == "Stop"
