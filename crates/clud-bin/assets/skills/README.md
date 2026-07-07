# skills/

Claude Code and Codex skills bundled into the `clud` binary as compile-time
assets. During global launch setup, the installer copies each skill into the
selected backend's skill directory (`~/.claude/skills/` for Claude,
`~/.codex/skills/` for Codex when Codex is present). Session-only launches
do not write persistent skill files. Stale clud-managed copies under
`~/.agents/skills/` are purged only during Codex global setup.

## Skills

- [clud-loop/](clud-loop/README.md) - Polyfill Claude-style `/loop` behavior
  for Codex with in-chat orchestration, a compact `.clud/loop/LOOP.md`
  ledger, bounded worker subagents, and explicit legacy external mode.
- [clud-issue/](clud-issue/README.md) - File a deeply-researched GitHub issue
  via investigate -> interview -> investigate -> post, returning a summary plus
  the issue URL.
- [clud-issue-triage/](clud-issue-triage/README.md) - Triage GitHub issues:
  close ones that are clearly resolved and silently file follow-ups for
  un-addressed CodeRabbit comments; supports single, last-week, or all.
- [clud-pr/](clud-pr/README.md) - Implement a GitHub issue, PR follow-up, or
  freeform task inside a `.claude/` worktree, or take an open PR through
  CI/review fixes to merge; code changes follow RED -> GREEN.
- [clud-fix/](clud-fix/README.md) - Drive a GitHub issue through PR merge,
  issue closure, and validation that the reported reproduction is fixed on
  main; code changes follow RED -> GREEN.
- [clud-tag-release/](clud-tag-release/README.md) - Tag a release after
  validating version match, clean `main`, and no duplicate tag, then push and
  surface the auto-release workflow URL.
- [clud-docker-rust-app-dev/](clud-docker-rust-app-dev/README.md) - Build a
  Rust app inside Docker for development iteration, not deployment. It uses
  fast incremental cargo builds via named volumes for `target/` + `CARGO_HOME`
  + `RUSTUP_HOME`, source bind-mounted, soldr-wrapped cargo, and a Python
  orchestrator.
- [clud-improve/](clud-improve/SKILL.md) - File concrete clud improvement
  reports directly as GitHub issues against `zackees/clud`; ask for details
  only on a bare manual `/clud-improve` invocation.

## How Skills Ship

Each `SKILL.md` here is embedded into the binary via `include_str!` and written
out during global setup. Two installer implementations are registered behind
`launch_setup.rs`:

- **`crates/clud-bin/src/skills.rs`** - selected-backend global setup
  (`~/.claude/skills`, `~/.codex/skills` gated by `~/.codex`), never
  overwrites user edits, reads from this directory.
- **`crates/clud-bin/src/skill_install.rs`** - Claude-only global setup,
  overwrites on semantic divergence, reads from a separate top-level `skills/`
  directory in the repo, and purges retired managed skills listed in
  `PURGED_SKILLS`.

The two source trees ship different subsets. See
[docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md)
for the full divergence map, rationale, and eventual consolidation plan
([DD-008](../../../../docs/DESIGN_DECISIONS.md#dd-008-dual-skill-installer-skillsrs-vs-skill_installrs--interim-state)).

## Adding a Skill

See the checklist in
[docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md#adding-a-skill).
Register with `skills.rs` for Codex coverage.
