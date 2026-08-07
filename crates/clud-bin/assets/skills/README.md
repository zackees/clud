# skills/

Claude Code and Codex skills bundled into the `clud` binary as compile-time
assets. During global launch setup, the installer copies each skill into the
selected backend's skill directory (`~/.claude/skills/` for Claude,
`~/.codex/skills/` for Codex when Codex is present). Session-only launches
do not write persistent skill files. Stale clud-managed copies under
`~/.agents/skills/` are purged only during Codex global setup.

Retired skills are listed in `skills::PURGED_BUNDLED_SKILLS` and swept out of
every backend's skills dir on launch. `clud-loop` was retired once
`clud --codex --harness claude` gave Codex models the harness's native `/loop`.

## Skills

- [clud-issue/](clud-issue/README.md) - File a deeply-researched GitHub issue
  via investigate -> interview -> investigate -> post, returning a summary plus
  the issue URL.
- [clud-issue-triage/](clud-issue-triage/README.md) - Triage GitHub issues:
  close ones that are clearly resolved and silently file follow-ups for
  un-addressed CodeRabbit comments; supports single, last-week, or all.
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
- [clud-docker-recover/](clud-docker-recover/SKILL.md) - Diagnose and recover
  a wedged Docker Desktop (engine pipe/socket absent, WSL/Docker startup
  failures) via the bundled `docker/docker_recover.py` tool. Read-only
  `doctor` first; confirmation-gated `restart`/`reset`; Windows storage disks
  resolved from Docker Desktop's real config (never the assumed C: default)
  and never compacted or deleted automatically.

## How Skills Ship

Each `SKILL.md` here is embedded into the binary via `include_str!` and written
out during global setup. One installer, registered behind `launch_setup.rs`:

- **`crates/clud-bin/src/skills.rs`** - selected-backend global setup
  (`~/.claude/skills`, `~/.codex/skills` gated by `~/.codex`), never
  overwrites user edits, reads from this directory.

See
[docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md)
for the installer model and rationale.

## Adding a Skill

See the checklist in
[docs/architecture/skill-system.md](../../../../docs/architecture/skill-system.md#adding-a-skill).
Register in `skills.rs::BUNDLED_SKILLS`.
