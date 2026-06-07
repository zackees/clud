# clud-issue/

Source of the `/clud-issue` skill shipped inside the `clud` binary. The skill drives a four-step workflow for filing a deeply-researched GitHub issue: silent round-1 investigation, mandatory user interview, round-2 deep dig, then `gh issue create`. It triggers when the user invokes `/clud-issue`, asks to "file an issue with research", or asks to draft an issue but needs scope clarified first. The deliverable is a posted issue URL plus a 2-3 sentence summary - never a draft left in chat.

## Files

- `SKILL.md` - Frontmatter (`name`, `description`, `triggers`) plus the workflow, failure modes, and "when not to use" sections that Claude Code reads when the skill fires.
- `README.md` - This file. Progressive-disclosure docs for contributors; not shipped to users.

## How it ships

`SKILL.md` is embedded into the `clud` binary at compile time via `include_str!` in two registries: `crates/clud-bin/src/skills.rs` (`BUNDLED_SKILLS`, which installs into the selected detected backend under `~/.claude/skills/` or `~/.codex/skills/`, never overwriting existing files) and `crates/clud-bin/src/skill_install.rs` (`BUNDLED_SKILLS`, which installs into `~/.claude/skills/clud-issue/SKILL.md` and overwrites on semantic divergence so the embedded copy stays canonical). Both run only during global setup and degrade silently on error - editing this file and rebuilding the binary is the only supported way to update what users see.
