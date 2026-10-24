"""Unit coverage for the bundled `git/state.py` tool (#1339 item 7)."""

from __future__ import annotations

import sys
from pathlib import Path

TOOL_DIR = Path(__file__).resolve().parents[1] / "crates" / "clud-bin" / "assets" / "tools" / "git"


def _load():
    if str(TOOL_DIR) not in sys.path:
        sys.path.insert(0, str(TOOL_DIR))
    import state

    return state


state = _load()

STATUS = """# branch.oid 0123456789abcdef0123456789abcdef01234567
# branch.head feat/x
# branch.upstream origin/feat/x
# branch.ab +2 -1
1 .M N... 100644 100644 100644 aaaa bbbb src/main.rs
2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.rs\told.rs
u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.rs
? notes.txt
"""


def test_status_parses_branch_upstream_and_every_entry_kind() -> None:
    parsed = state.parse_status(STATUS)
    assert parsed["branch"] == "feat/x"
    assert parsed["head"] == "0123456789ab"
    assert (parsed["upstream"], parsed["ahead"], parsed["behind"]) == ("origin/feat/x", 2, 1)
    assert parsed["changes"] == [
        {"xy": ".M", "path": "src/main.rs"},
        {"xy": "R.", "path": "new name.rs"},
        {"xy": "UU", "path": "conflict.rs"},
        {"xy": "??", "path": "notes.txt"},
    ]


def test_a_detached_head_without_upstream() -> None:
    parsed = state.parse_status("# branch.oid abcdefabcdefabcdef\n# branch.head (detached)\n")
    assert parsed["branch"] is None
    assert parsed["upstream"] is None
    parsed.update(commits=[], stashes=[], worktrees=[])
    assert "branch: (detached at abcdefabcdef)" in state.render(parsed)


def test_worktrees_with_a_stash_render_together() -> None:
    trees = state.parse_worktrees(
        "worktree /r\nHEAD aa\nbranch refs/heads/main\n\nworktree /r/wt\nHEAD bb\ndetached\n"
    )
    assert trees == [{"path": "/r", "branch": "main"}, {"path": "/r/wt", "branch": None}]
    parsed = state.parse_status(STATUS)
    parsed.update(commits=["abc fix"], stashes=["stash@{0} WIP"], worktrees=trees)
    text = state.render(parsed)
    assert "upstream: origin/feat/x ahead 2 behind 1" in text
    assert "stashes: 1" in text
    assert "/r/wt [detached]" in text
