# Skill System

Skills are slash-command playbooks (`/clud-pr`, `/clud-issue`, etc.) that ship
embedded inside the `clud` binary as compile-time string assets. Persistent
skill installation happens only during global launch setup. Session-only
launches do not write agent setup files.

Claude skills are written under `~/.claude/skills/`. Codex skills are
written under `~/.codex/skills/`, mirroring Claude's layout (gated on
`~/.codex` existing). Clud-managed copies that an older build wrote to
`~/.agents/skills/` are purged best-effort during Codex global setup.

## Component Map

| Component | Path | Role |
|---|---|---|
| Multi-backend installer | `crates/clud-bin/src/skills.rs` | Installs selected-backend skills only when the target file is missing. |
| Claude drift installer | `crates/clud-bin/src/skill_install.rs` | Installs Claude-owned top-level skills and overwrites on semantic divergence. |
| Multi-backend source tree | `crates/clud-bin/assets/skills/<name>/` | Holds `SKILL.md` plus contributor `README.md` files consumed by `skills.rs`. |
| Top-level source tree | `skills/<name>/SKILL.md` | Holds skills consumed by `skill_install.rs`. |
| `BUNDLED_SKILLS` | `skills.rs`, `skill_install.rs` | Compile-time `include_str!` registries with different element types and source trees. |
| `PURGED_SKILLS` | `skill_install.rs` | Retired Claude-only skill names safely removed from managed installs. |
| Launch setup gate | `launch_setup.rs` | Selects session-only vs global setup and dispatches selected-backend setup actions. |

## Why Two Installers

The two installers are an interim historical reality, not a designed-in split.
`skill_install.rs` predates `skills.rs` and was kept when the broader
multi-backend expander landed.

| Concern | `skills.rs` | `skill_install.rs` |
|---|---|---|
| Backends targeted | Selected backend during global setup; Claude plus Codex today | Claude global setup only |
| Source tree | `crates/clud-bin/assets/skills/` | Top-level `skills/` |
| Existing file behavior | Skip, preserving user edits | Compare modulo whitespace; overwrite semantic divergence |
| Bundled skills | `clud-loop`, `clud-issue`, `clud-issue-triage`, `clud-pr`, `clud-fix`, `clud-tag-release`, `clud-docker-rust-app-dev`, `clud-windows-trash`, `clud-extern-repos`, `clud-improve` | `clud-pr`, `clud-issue`, `clud-windows-trash`, `clud-extern-repos` |
| Retired purge list | Stale clud-managed copies under `~/.agents/skills/` | `clud-pr-merge` |

Both flows are non-fatal. A failure logs a `[clud] note: ...` line and launch
continues.

## Global Setup Flow

`main()` resolves the backend, asks `launch_setup.rs` for a setup scope, and
then builds the final `LaunchPlan`. If `~/.clud/settings.toml` contains a
backend-level global preference, future launches for that backend run global
setup without prompting. Otherwise automation, piped stdin, `--dry-run`, and
one-shot prompt launches default to session-only. Bare interactive launches can
opt into global setup; choosing global persists that preference, while choosing
session-only remains per-launch.

When global setup is selected:

1. `skills::ensure_installed_for_backend()` runs for the selected backend. For
   Codex, it first purges stale clud-managed `~/.agents/skills/` copies and
   then writes missing skills to `~/.codex/skills/`.
2. `skill_install::ensure_installed()` runs only for Claude global setup. It
   installs or updates the Claude-owned skills and then walks `PURGED_SKILLS`,
   removing retired managed skill directories.

## Source-Tree Divergence

| Skill | `assets/skills/` (`skills.rs`) | top-level `skills/` (`skill_install.rs`) |
|---|---|---|
| `clud-loop` | yes | no |
| `clud-issue` | yes | yes |
| `clud-pr` | yes | yes |
| `clud-fix` | yes | no |
| `clud-issue-triage` | yes | no |
| `clud-tag-release` | yes | no |
| `clud-docker-rust-app-dev` | yes | no |
| `clud-windows-trash` | yes | yes |
| `clud-extern-repos` | yes | yes |
| `clud-pr-merge` | retired into `clud-pr` merge mode | purged |

`clud-pr-merge` was folded into `clud-pr` as PR merge mode. The old standalone
name remains in `PURGED_SKILLS` so managed installs do not linger.

## Adding Or Retiring A Skill

1. Choose the installer. Multi-backend coverage requires `skills.rs`;
   drift-on-divergence semantics require `skill_install.rs`.
2. Add the `SKILL.md` source file with YAML frontmatter and the
   `<!-- managed-by: clud -->` marker.
3. Register the file in the relevant `BUNDLED_SKILLS` constant.
4. Link contributor docs from `crates/clud-bin/assets/skills/README.md` when
   the skill lives under `assets/skills/`.
5. If a skill is merged or removed after it may have been installed, delete its
   bundle entry and add the old name to `PURGED_SKILLS`. Purge code deletes
   only managed skill directories.
6. Run `bash lint` and `bash test`. Bundle tests assert non-empty content,
   unique names, the managed marker, and the RED -> GREEN code-change rule.

## Key Types / Constants

- `BundledSkill` (`skills.rs`): public struct with `name` and `skill_md`.
- `Skill` (`skill_install.rs`): private struct with `name` and `content`.
- `SKILL_BACKENDS` (`skills.rs`): selected backend install gates and target
  directories. Codex uses `.codex` as both the gate and the skills root
  (`~/.codex/skills/`), mirroring Claude.
- `InstallReport` and `LegacyPurgeReport` (`skills.rs`): setup and stale
  cleanup summaries.
- `PURGED_SKILLS` (`skill_install.rs`): retired Claude-only names. Removal
  only proceeds when `SKILL.md` still contains `managed-by: clud`.

## Consolidation Plan

The dual installer remains interim state. One small consolidation has landed:
`/clud-pr-merge` is now `/clud-pr` PR merge mode, and the old standalone name
is purged from managed Claude installs. Future work should collapse the
remaining duplication into a single installer with a single source tree while
preserving Codex coverage, Claude drift detection, and safe retirement.

## See Also

- `../../crates/clud-bin/assets/skills/README.md`
- `launch-setup.md`
- `../DESIGN_DECISIONS.md` (DD-008)
