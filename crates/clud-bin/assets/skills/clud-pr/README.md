# clud-pr/

Bundled skill that implements a GitHub issue, PR follow-up, or freeform task inside a `.claude/worktrees/<branch>/` worktree and ships it as one clean PR. Triggers on `/clud-pr`, a PR URL/number (triage mode), an issue URL/number, or any phrasing like "ship", "do-pr", or a task sentence with intent to deliver. The end product is a pushed PR (via `gh pr create`), a removed worktree, and a clean main checkout.

## Files

- `SKILL.md` — Frontmatter, triggers, and the full playbook (worktree gating, PR triage mode, task-to-PR workflow, failure modes).

## How it ships

The file is embedded into the `clud` binary at compile time via `include_str!("../assets/skills/clud-pr/SKILL.md")` from `crates/clud-bin/src/skills.rs` (entry in `BUNDLED_SKILLS`). On every `clud` launch, `ensure_installed` walks each backend in `SKILL_BACKENDS` (Claude Code at `~/.claude`, Codex at `~/.codex`) whose home subdir already exists and writes `~/<home>/skills/clud-pr/SKILL.md` only when the target file is missing — existing user edits are preserved. A parallel installer in `crates/clud-bin/src/skill_install.rs` additionally enforces the embedded copy as source of truth: whitespace-only differences are a no-op, but semantic divergence is overwritten and logged as `[clud] updated /clud-pr`. All install errors are non-fatal.
