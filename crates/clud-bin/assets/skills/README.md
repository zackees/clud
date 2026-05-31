# skills/

Claude Code "skills" bundled into the `clud` binary as compile-time assets. On every `clud` launch the installer copies each skill into the user's `~/.claude/skills/` (and `~/.codex/skills/` when that backend is present), so a fresh `clud` install always carries the current canonical playbooks without any extra setup step.

## Skills

- [clud-issue/](clud-issue/README.md) — File a deeply-researched GitHub issue via investigate → interview → investigate → post, returning a summary plus the issue URL.
- [clud-issue-triage/](clud-issue-triage/README.md) — Triage GitHub issues: close ones that are clearly resolved and silently file follow-ups for un-addressed CodeRabbit comments; supports single, last-week, or all (parallel sub-agents in worktrees).
- [clud-pr/](clud-pr/README.md) — Implement a GitHub issue, PR follow-up, or freeform task inside a `.claude/` worktree and ship one clean PR.
- [clud-tag-release/](clud-tag-release/README.md) — Tag a release after validating version match, clean `main`, and no duplicate tag, then push and surface the auto-release workflow URL.
- [clud-docker-rust-app/](clud-docker-rust-app/README.md) — Containerize a Rust app for fast incremental Docker builds — named volumes for `target/` + `CARGO_HOME` + `RUSTUP_HOME`, source bind-mounted, soldr-wrapped cargo, and a Python orchestrator. No-op rebuilds drop from minutes to seconds, especially on Windows/WSL2.

## How skills ship

Each `SKILL.md` here is embedded into the binary via `include_str!` and written out on launch. Two installers run on every launch:

- **`crates/clud-bin/src/skills.rs`** — multi-backend (`~/.claude`, `~/.codex`), never overwrites user edits, reads from this directory.
- **`crates/clud-bin/src/skill_install.rs`** — Claude-only, overwrites on semantic divergence, reads from a separate top-level `skills/` directory in the repo.

The two source trees ship different subsets — see [docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md) for the full divergence map, rationale, and the eventual consolidation plan ([DD-008](../../../../docs/DESIGN_DECISIONS.md#dd-008-dual-skill-installer-skillsrs-vs-skill_installrs--interim-state)).

## Adding a skill

See the checklist in [docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md#adding-a-skill) — it covers which installer to register with, where to place `SKILL.md`, the unit-test invariants, and the README expectation. Confirm both installers are updated if the skill should ship to Codex as well.
