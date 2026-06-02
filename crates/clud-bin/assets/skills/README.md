# skills/

Claude Code and Codex "skills" bundled into the `clud` binary as compile-time assets. On every `clud` launch the installer copies each skill into the user's `~/.claude/skills/` and, when Codex is present, Codex's current `~/.agents/skills/` path, so a fresh `clud` install always carries the current canonical playbooks without any extra setup step. Older clud-managed copies under `~/.codex/skills/` are purged on launch.

## Skills

- [clud-issue/](clud-issue/README.md) — File a deeply-researched GitHub issue via investigate → interview → investigate → post, returning a summary plus the issue URL.
- [clud-issue-triage/](clud-issue-triage/README.md) — Triage GitHub issues: close ones that are clearly resolved and silently file follow-ups for un-addressed CodeRabbit comments; supports single, last-week, or all (parallel sub-agents in worktrees).
- [clud-pr/](clud-pr/README.md) — Implement a GitHub issue, PR follow-up, or freeform task inside a `.claude/` worktree and ship one clean PR.
- [clud-tag-release/](clud-tag-release/README.md) — Tag a release after validating version match, clean `main`, and no duplicate tag, then push and surface the auto-release workflow URL.
- [clud-docker-rust-app-dev/](clud-docker-rust-app-dev/README.md) — Build a Rust app inside Docker for **development iteration** (not deployment) — fast incremental cargo builds via named volumes for `target/` + `CARGO_HOME` + `RUSTUP_HOME`, source bind-mounted, soldr-wrapped cargo, and a Python orchestrator. The `-dev` suffix is load-bearing: this is a per-developer scratch container, not a `docker push`-bound image; use `cargo chef` / multi-stage builds for that path.

## How skills ship

Each `SKILL.md` here is embedded into the binary via `include_str!` and written out on launch. Two installers run on every launch:

- **`crates/clud-bin/src/skills.rs`** — multi-backend (`~/.claude/skills`, Codex `~/.agents/skills` gated by `~/.codex`), never overwrites user edits, reads from this directory.
- **`crates/clud-bin/src/skill_install.rs`** — Claude-only, overwrites on semantic divergence, reads from a separate top-level `skills/` directory in the repo.

The two source trees ship different subsets — see [docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md) for the full divergence map, rationale, and the eventual consolidation plan ([DD-008](../../../../docs/DESIGN_DECISIONS.md#dd-008-dual-skill-installer-skillsrs-vs-skill_installrs--interim-state)).

## Adding a skill

See the checklist in [docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md#adding-a-skill) — it covers which installer to register with, where to place `SKILL.md`, the unit-test invariants, and the README expectation. Register with `skills.rs` for Codex coverage.
