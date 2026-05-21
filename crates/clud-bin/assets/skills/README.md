# skills/

Claude Code "skills" bundled into the `clud` binary as compile-time assets. On every `clud` launch the installer copies each skill into the user's `~/.claude/skills/` (and `~/.codex/skills/` when that backend is present), so a fresh `clud` install always carries the current canonical playbooks without any extra setup step.

## Skills

- [clud-issue/](clud-issue/README.md) — File a deeply-researched GitHub issue via investigate → interview → investigate → post, returning a summary plus the issue URL.
- [clud-issue-triage/](clud-issue-triage/README.md) — Triage GitHub issues: close ones that are clearly resolved and silently file follow-ups for un-addressed CodeRabbit comments; supports single, last-week, or all (parallel sub-agents in worktrees).
- [clud-pr/](clud-pr/README.md) — Implement a GitHub issue, PR follow-up, or freeform task inside a `.claude/` worktree and ship one clean PR.
- [clud-tag-release/](clud-tag-release/README.md) — Tag a release after validating version match, clean `main`, and no duplicate tag, then push and surface the auto-release workflow URL.

## How skills ship

Each `SKILL.md` here is embedded into the binary via `include_str!` and written out on launch. Two installer paths exist and contributors should be aware of both:

- **`crates/clud-bin/src/skills.rs`** is the multi-backend installer. It iterates `BUNDLED_SKILLS` (sourced from this directory) and `SKILL_BACKENDS` (currently `~/.claude` and `~/.codex`), writes only when the target `SKILL.md` is missing, and never overwrites user edits.
- **`crates/clud-bin/src/skill_install.rs`** is a Claude-only installer that reads from a separate top-level `skills/` directory in the repo (not this one) and *does* overwrite when content diverges semantically from the embedded copy (whitespace/CRLF differences are tolerated).

Both run on every launch and degrade silently on error. If you change a bundled skill, confirm which installer (or both) owns the on-disk copy.

## Adding a skill

- Create `assets/skills/<name>/SKILL.md` with the standard frontmatter (`name:`, `description:`, `triggers:`) and the `<!-- managed-by: clud -->` marker.
- Append a `BundledSkill { name, skill_md: include_str!("../assets/skills/<name>/SKILL.md") }` entry to `BUNDLED_SKILLS` in `crates/clud-bin/src/skills.rs`.
- Decide whether the skill also needs to ship via `crates/clud-bin/src/skill_install.rs` (overwriting installer, top-level `skills/<name>/SKILL.md` source) and update that bundle list if so.
- Add a short `README.md` next to the new `SKILL.md` and link it from the **Skills** section above.
- Run `bash lint` and `bash test`; the existing unit tests assert the bundle is non-empty, names are unique, and every entry carries the `managed-by: clud` marker.
