# clud-issue/

Source of the `/clud-issue` skill shipped inside the `clud` binary. Ordinary issue filing uses two investigation rounds, then `gh issue create`; judgment calls go in the issue body's `## Decisions` section rather than a user interview. A request to create a meta issue, roll up issues, or combine them under a parent activates the native hierarchy mode: inspect existing parents, attach existing children through GitHub's sub-issues API, and verify both directions. Body links alone are not completion. The deliverable is the posted parent URL and an honest attachment summary, including any partial failure.

## Files

- `SKILL.md` - Frontmatter (`name`, `description`, `triggers`) plus the workflow, failure modes, and "when not to use" sections that Claude Code reads when the skill fires.
- `README.md` - This file. Progressive-disclosure docs for contributors; not shipped to users.

## How it ships

`SKILL.md` is embedded into the `clud` binary at compile time via `include_str!` from the single registry `crates/clud-bin/src/skills.rs` (`BUNDLED_SKILLS`), which installs into the selected detected backend under `~/.claude/skills/` or `~/.codex/skills/`, never overwriting existing files. It runs only during global setup and degrades silently on error - editing this file and rebuilding the binary is the only supported way to update what users see.
