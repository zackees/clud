# clud-improve/

Source of the `/clud-improve` skill shipped inside the `clud` binary. The skill files a concrete clud improvement report directly when the invocation includes an argument string or the current user message already contains the report. It asks the literal question "how can clud improve? be as specific as possible" only for a bare manual `/clud-improve` invocation without details. It checks `gh auth status`, and on success files the report as a GitHub issue against `zackees/clud`. If the user is not authenticated, the skill tells them to run `gh auth login` and stops. The deliverable is a posted issue URL plus a one-sentence summary - never a draft left in chat.

## Files

- `SKILL.md` - Frontmatter (`name`, `description`, `triggers`) plus the workflow, failure modes, and "when not to use" sections that Claude Code reads when the skill fires.
- `README.md` - This file. Progressive-disclosure docs for contributors; not shipped to users.

## How it ships

`SKILL.md` is embedded into the `clud` binary at compile time via `include_str!` in `crates/clud-bin/src/skills.rs` (`BUNDLED_SKILLS`). During global setup it is installed into the selected backend's skill directory (`~/.claude/skills/clud-improve/SKILL.md` for Claude, `~/.codex/skills/clud-improve/SKILL.md` for Codex when `.codex` exists). Existing files are preserved so user edits survive. Editing this file and rebuilding the binary is the only supported way to update what users see.
