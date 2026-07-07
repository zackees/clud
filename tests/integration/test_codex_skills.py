"""Integration tests covering Codex bundled-skill install behavior.

The full ``clud --codex`` install path runs only when launch setup elects the
``Global`` scope, which requires an interactive TTY on both stdin and stderr
(see ``setup_interactive`` in ``crates/clud-bin/src/main.rs``). Subprocess
tests cannot easily synthesize that, so the byte-for-byte install assertions
live as Rust unit tests in ``crates/clud-bin/src/skills.rs::tests`` and the
end-to-end launch-setup flow lives in
``crates/clud-bin/src/launch_setup.rs::tests``. This file holds the
subprocess-scope checks: the binary builds, parses ``--codex`` flag-paths
without crashing, and the embedded ``SKILL.md`` source matches what the
installer would write.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.integration

REPO_ROOT = Path(__file__).resolve().parents[2]
BUNDLED_CLUD_PR_SKILL_PATH = (
    REPO_ROOT / "crates" / "clud-bin" / "assets" / "skills" / "clud-pr" / "SKILL.md"
)
BUNDLED_CLUD_FIX_SKILL_PATH = (
    REPO_ROOT / "crates" / "clud-bin" / "assets" / "skills" / "clud-fix" / "SKILL.md"
)


def test_bundled_clud_pr_skill_carries_managed_marker_and_frontmatter() -> None:
    """The source-of-truth SKILL.md must carry the marker the installer keys
    its purge on. If this drifts, every install/purge guarantee in
    ``skills.rs`` silently weakens."""
    body = BUNDLED_CLUD_PR_SKILL_PATH.read_text(encoding="utf-8")
    assert body.startswith("---"), "SKILL.md must begin with YAML frontmatter"
    assert "<!-- managed-by: clud -->" in body, (
        "managed-by marker is required for purge_stale_agents_skills "
        "to recognize clud-managed copies"
    )


def test_bundled_clud_fix_skill_carries_codex_install_metadata() -> None:
    """Issue #353: clud-fix must be a multi-backend bundled skill so Codex
    installs it under ~/.codex/skills just like the other clud skills."""
    body = BUNDLED_CLUD_FIX_SKILL_PATH.read_text(encoding="utf-8")
    assert body.startswith("---"), "SKILL.md must begin with YAML frontmatter"
    assert "name: clud-fix" in body
    assert "<!-- managed-by: clud -->" in body
    assert "RED -> GREEN" in body
    assert "clud-pr-merge" not in body
    assert "/goal $clud-fix <issue-or-issue-url>" in body
    assert "Complete meta issue #N" in body
    assert "every child issue closed/validated" in body
    assert "parent issue closed" in body
    assert ".clud/fix/<owner>__<repo>__issue-<num>.json" in body


def test_clud_codex_dry_run_does_not_crash(
    clud_binary: Path, mock_env: dict[str, str], tmp_path: Path
) -> None:
    """A ``--codex --dry-run -p`` invocation must parse and emit a launch plan
    without writing skill files. This guards against regressions where the
    Codex backend path is broken in arg parsing or LaunchPlan construction."""
    result = subprocess.run(
        [str(clud_binary), "--codex", "--dry-run", "-p", "noop prompt"],
        capture_output=True,
        text=True,
        timeout=30,
        env=mock_env,
        cwd=tmp_path,
    )
    assert result.returncode == 0, f"stderr: {result.stderr}\nstdout: {result.stdout}"
    # --dry-run emits a JSON LaunchPlan to stdout.
    assert '"backend"' in result.stdout, f"missing backend field: {result.stdout!r}"
    assert "codex" in result.stdout.lower()
    # Dry-run is a non-prompting launch, so no global-setup files should land.
    home_env = mock_env.get("HOME") or mock_env.get("USERPROFILE")
    if home_env:
        # We did not pre-create ~/.codex, and dry-run uses session-only scope,
        # so the install path should never touch a real home dir.
        assert not (tmp_path / ".codex" / "skills").exists()
        assert not (tmp_path / ".agents" / "skills").exists()
