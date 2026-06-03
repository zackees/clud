# clud-pr/

Bundled skill that implements a GitHub issue, PR follow-up, or freeform task inside a `.claude/worktrees/<branch>/` worktree and ships it as one clean PR. It also owns PR merge mode, formerly `/clud-pr-merge`, for taking an open PR through CI/review fixes to merge. Triggers on `/clud-pr`, merge/land requests, a PR URL/number (triage mode), an issue URL/number, or any phrasing like "ship", "do-pr", or a task sentence with intent to deliver. Code changes follow RED -> GREEN: failing focused test/repro first, implementation second, passing signal before broad gates.

## Files

- `SKILL.md` - Frontmatter, triggers, and the full playbook (worktree gating, PR merge mode, PR triage mode, task-to-PR workflow, failure modes).

## How it ships

The file is embedded into the `clud` binary at compile time via `include_str!("../assets/skills/clud-pr/SKILL.md")` from `crates/clud-bin/src/skills.rs` (entry in `BUNDLED_SKILLS`). During global setup, `ensure_installed` writes `<skills_dir>/clud-pr/SKILL.md` for the selected backend only when the target file is missing: Claude Code under `~/.claude/skills`, and Codex under the current `~/.agents/skills` path gated by `~/.codex`. Existing user edits are preserved. A parallel Claude-only installer in `crates/clud-bin/src/skill_install.rs` enforces the embedded copy as source of truth: whitespace-only differences are a no-op, semantic divergence is overwritten and logged as `[clud] updated /clud-pr`, and retired managed `/clud-pr-merge` installs are purged through `PURGED_SKILLS`. All install errors are non-fatal.
