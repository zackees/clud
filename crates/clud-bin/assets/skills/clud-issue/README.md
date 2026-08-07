# clud-issue/

Source of the `/clud-issue` skill shipped inside the `clud` binary. The skill drives a workflow for filing a deeply-researched GitHub issue without interviewing the user: silent round-1 investigation, then the agent answers the open questions itself from code/history/docs, then a round-2 deep dig, then `gh issue create`. Judgment calls (priority, scope edges, fix location, acceptance criteria) are decided by the agent and recorded in a `## Decisions` section of the issue body, with anything genuinely unresolvable listed under `Open questions` so the user can correct it on GitHub. It triggers when the user invokes `/clud-issue`, asks to "file an issue with research", or wants an issue filed and expects the agent to resolve scope itself. The deliverable is a posted issue URL plus a 2-3 sentence summary - never a draft left in chat.

## Files

- `SKILL.md` - Frontmatter (`name`, `description`, `triggers`) plus the workflow, failure modes, and "when not to use" sections that Claude Code reads when the skill fires.
- `README.md` - This file. Progressive-disclosure docs for contributors; not shipped to users.

## How it ships

`SKILL.md` is embedded into the `clud` binary at compile time via `include_str!` from the single registry `crates/clud-bin/src/skills.rs` (`BUNDLED_SKILLS`), which installs into the selected detected backend under `~/.claude/skills/` or `~/.codex/skills/`, never overwriting existing files. It runs only during global setup and degrades silently on error - editing this file and rebuilding the binary is the only supported way to update what users see.
