# clud-issue-triage/

Bundled skill that triages GitHub issues for the user - closing only issues that a merged PR on the default branch unambiguously resolves, and silently filing follow-up issues for substantive un-addressed CodeRabbit comments. Triggered when the user types `/clud-issue-triage` (optionally with an issue number/URL or `all`), or asks to "triage issues", "sweep stale issues", or "clean up the issue tracker". Supports three modes: single-issue (handled directly), last-7-days bulk (no arg), and `all` open issues; bulk modes dispatch one parallel sub-agent per issue inside disposable `.claude/worktrees/triage-<num>/` worktrees and tear them down on completion. Output is two lines per issue: closure decision plus one-line evidence, and any follow-up issues filed.

## Files

- `SKILL.md` - Frontmatter (`name`, `description`, `triggers`) plus the playbook the agent loads when the skill fires: single-issue and bulk workflows, failure modes, and when to defer to `/clud-issue` or `/clud-pr`.

## How it ships

The skill's `SKILL.md` is embedded into the `clud` binary at compile time via `include_str!` from `crates/clud-bin/src/skills.rs` (entry in `BUNDLED_SKILLS`). During global setup, `skills::ensure_installed` writes `<skills_dir>/clud-issue-triage/SKILL.md` for the selected backend only when missing: Claude Code under `~/.claude/skills`, and Codex under `~/.codex/skills` gated by `~/.codex`. Existing user edits are preserved. Install errors are non-fatal and log to stderr without blocking launch.
