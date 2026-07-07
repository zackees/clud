# clud-fix/

Bundled skill that drives a single GitHub issue or a meta/parent/burn-down issue
through the full fix lifecycle: implementation PRs, merge to the default branch,
child issue closure, parent checklist updates, parent closure, and validation
that reported reproductions no longer fail. It owns the outer issue-level
`/goal` and uses `clud-pr` in delegated mode for worktree, PR, CI/review, and
merge-mode mechanics. Code changes follow RED -> GREEN: focused failing test or
repro first, implementation second, passing focused signal before broad gates.

## Files

- `SKILL.md` - Frontmatter, triggers, and the full orchestration playbook.

## How it ships

The file is embedded into the `clud` binary at compile time via
`include_str!("../assets/skills/clud-fix/SKILL.md")` from
`crates/clud-bin/src/skills.rs` (entry in `BUNDLED_SKILLS`). During global
setup, `ensure_installed` writes `<skills_dir>/clud-fix/SKILL.md` for the
selected backend only when the target file is missing: Claude Code under
`~/.claude/skills`, and Codex under `~/.codex/skills` gated by `~/.codex`.
The Claude drift installer also includes `clud-fix` so stale managed Claude
copies are upgraded to the canonical workflow. All install errors are non-fatal.
