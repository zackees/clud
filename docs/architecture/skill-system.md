# Skill System

Skills are slash-command playbooks (`/clud-issue`, `/clud-review`, etc.) that ship
embedded inside the `clud` binary as compile-time string assets. Persistent
skill installation happens only during global launch setup. Session-only
launches do not write agent setup files.

Claude skills are written under `~/.claude/skills/`. Codex skills are
written under `~/.codex/skills/`, mirroring Claude's layout (gated on
`~/.codex` existing). Clud-managed copies that an older build wrote to
`~/.agents/skills/` are purged best-effort during Codex global setup.

## One Source Of Truth

There is exactly **one** skill source tree and **one** installer:

- Source tree: `crates/clud-bin/assets/skills/<name>/SKILL.md`
- Installer: `crates/clud-bin/src/skills.rs`

This is load-bearing, not incidental. clud previously had a second registry
(`skill_install.rs`) reading a second tree at the repo root, and both wrote the
same `~/.claude/skills/` files during the same startup. Each pass compared the
file on disk against *its own* embedded copy, classified the other's output as
drift, and rewrote it — so every launch printed `updated /clud-pr` and
`updated /clud-issue`, the newer `assets/` bodies were silently reverted to
older root copies, and Codex ended up with different content than Claude.

Neither installer was wrong in isolation; nothing enforced that they owned
disjoint names. Full rationale in
[DD-039](../DESIGN_DECISIONS.md#dd-039-bundled-skills-have-exactly-one-source-of-truth).

`ci/banned_skill_sources.py` (run by `bash lint`) enforces the invariant:

1. Skill bodies may only be `include_str!`'d from `assets/skills/`.
2. Only `skills.rs` / `skills_home.rs` may build a backend skills path.
3. No second skill source tree at the repo root.

Rule 1 alone would have caught the original bug. A line-scoped
`skill-source-lint: allow` marker exempts prose and test assertions that name
these shapes without being writers; `rg 'skill-source-lint: allow'` lists every
escape in the tree.

## Component Map

| Component | Path | Role |
|---|---|---|
| Installer | `crates/clud-bin/src/skills.rs` | Installs selected-backend skills; refreshes stale clud-managed copies, never touches user-owned ones. |
| Source tree | `crates/clud-bin/assets/skills/<name>/` | Holds `SKILL.md` plus contributor `README.md` files. The only source of skill bodies. |
| `BUNDLED_SKILLS` | `skills.rs` | Compile-time `include_str!` registry of every shipped skill. |
| `PURGED_BUNDLED_SKILLS` | `skills.rs` | Retired names swept from *every* backend's skills dir. |
| Home resolution | `crates/clud-bin/src/skills_home.rs` | Resolves the user home dir that `skills.rs` joins onto. |
| Launch setup gate | `launch_setup.rs` | Selects session-only vs global setup and dispatches setup actions. |
| Source lint | `ci/banned_skill_sources.py` | Fails `bash lint` if a second tree or installer appears. |

Installation is non-fatal. A failure logs a `[clud] note: ...` line and launch
continues.

## Global Setup Flow

`main()` resolves the backend, asks `launch_setup.rs` for a setup scope, and
then builds the final `LaunchPlan`. If `~/.clud/settings.json` contains a
backend-level global preference, future launches for that backend run global
setup without prompting. Otherwise automation, piped stdin, `--dry-run`, and
one-shot prompt launches default to session-only. Bare interactive launches can
opt into global setup; choosing global persists that preference, while choosing
session-only remains per-launch.

When global setup is selected, `skills::ensure_installed_for_backend()` runs
for the selected backend — a single pass, which is what keeps the steady state
silent:

- For Codex it first purges stale clud-managed `~/.agents/skills/` copies, then
  writes missing skills to `~/.codex/skills/`.
- Either backend sweeps `PURGED_BUNDLED_SKILLS` out of *every* backend's skills
  dir, so a skill retired while the user was on Codex still goes away for a
  Claude-only user.

## Install Contract

`install_to` classifies each `<name>/SKILL.md` into one of four states, and
only one of them writes to an existing file:

| On-disk state | Action | Report field |
|---|---|---|
| Missing | Write the embedded copy | `installed` |
| Marker stripped (user-owned) | Never touched — deleting `managed-by: clud` is how a user claims a copy | `skipped_existing` |
| Marker present, semantically stale | Overwrite with the embedded copy | `refreshed` |
| Current (modulo whitespace) | **No write at all** | `skipped_existing` |

Comparison collapses whitespace runs (`normalize()`), so an LF-vs-CRLF
difference is not a change — the deliberate price is that a whitespace-only
edit to a bundled skill does not propagate to installed homes
([DD-040](../DESIGN_DECISIONS.md#dd-040-clud-pr-clud-fix-clud-do-and-clud-pr-merge-are-retired-in-favor-of-goal)).
`BundledSkillsAction` announces `[clud] updated /<name>` only for `refreshed`
entries; `installed` and `skipped_existing` are silent, so a repeat launch
prints nothing. `real_bundle_install_is_idempotent` and
`line_ending_drift_is_not_a_refresh` (in `skills_tests.rs`) pin both halves.

## Adding Or Retiring A Skill

1. Add `crates/clud-bin/assets/skills/<name>/SKILL.md` with YAML frontmatter
   and the `<!-- managed-by: clud -->` marker. There is no second tree to
   choose between, and putting the file anywhere else fails `bash lint`.
2. Register it in `BUNDLED_SKILLS` in `skills.rs`.
3. Link contributor docs from `crates/clud-bin/assets/skills/README.md`.
4. To retire a skill after users may have installed it, delete its bundle entry
   *and* add the old name to `PURGED_BUNDLED_SKILLS`. Removing the entry alone
   stops new installs but leaves every existing copy in place. Purge deletes
   only directories whose `SKILL.md` still carries `managed-by: clud`.
5. Run `bash lint` and `bash test`. Bundle tests assert non-empty content,
   unique names, valid frontmatter, the managed marker, and the
   RED -> GREEN code-change rule.

## Key Types / Constants

- `BundledSkill` (`skills.rs`): public struct with `name` and `skill_md`.
- `SKILL_BACKENDS` (`skills.rs`): per-backend install gates and target
  directories. Codex uses `.codex` as both the gate and the skills root
  (`~/.codex/skills/`), mirroring Claude.
- `InstallReport` and `LegacyPurgeReport` (`skills.rs`): setup and stale
  cleanup summaries.
- `PURGED_BUNDLED_SKILLS` (`skills.rs`): retired names. Deliberately an
  explicit list rather than "sweep anything not in `BUNDLED_SKILLS`" — an
  orphan sweep would delete still-used skills installed by a since-removed
  bundler, and would delete newer skills whenever an older binary ran.
  Retirement is a decision, not an inference.

## See Also

- `../../crates/clud-bin/assets/skills/README.md`
- `launch-setup.md`
- `../DESIGN_DECISIONS.md` (DD-008, DD-039, DD-040 — the retirement of
  `clud-pr`, `clud-fix`, `clud-do`, `clud-pr-merge` in favor of `/goal`)
