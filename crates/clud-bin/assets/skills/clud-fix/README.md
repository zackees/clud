# clud-fix/

Bundled skill that drives a GitHub issue through the full fix lifecycle:
implementation PR, merge to the default branch, issue closure, and validation
that the reported reproduction no longer fails. It uses `clud-pr` for worktree,
PR, CI/review, and merge-mode mechanics. Code changes follow RED -> GREEN:
focused failing test or repro first, implementation second, passing focused
signal before broad gates.

## Files

- `SKILL.md` - Frontmatter, triggers, and the full orchestration playbook.

## How it ships

The file is embedded into the `clud` binary at compile time via
`include_str!("../assets/skills/clud-fix/SKILL.md")` from
`crates/clud-bin/src/skills.rs` (entry in `BUNDLED_SKILLS`). During global
setup, `ensure_installed` writes `<skills_dir>/clud-fix/SKILL.md` for the
selected backend only when the target file is missing: Claude Code under
`~/.claude/skills`, and Codex under `~/.codex/skills` gated by `~/.codex`.
Existing user edits are preserved. All install errors are non-fatal.
