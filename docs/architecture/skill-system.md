# Skill System

Skills are slash-command playbooks (`/clud-pr`, `/clud-issue`, etc.) that ship
embedded inside the `clud` binary as compile-time string assets and are
installed into the user's backend skill directories (`~/.claude/skills/` and
Codex's current `~/.agents/skills/`) on every launch. A fresh `clud` install
always carries the current canonical copy of every bundled skill without any
extra setup step. Older clud-managed Codex copies in `~/.codex/skills/` are
purged best-effort during the same launch-time pass.

## Component map

| Component | Path | Role |
|---|---|---|
| Multi-backend installer | `crates/clud-bin/src/skills.rs` | Iterates `BUNDLED_SKILLS` and `SKILL_BACKENDS`; writes only when the target file is missing. |
| Claude-only drift installer | `crates/clud-bin/src/skill_install.rs` | Reads from top-level `skills/`; overwrites on semantic divergence. |
| Multi-backend source tree | `crates/clud-bin/assets/skills/<name>/` | Holds `SKILL.md` + `README.md` for skills consumed by `skills.rs`. |
| Top-level source tree | `skills/<name>/SKILL.md` | Holds skills consumed by `skill_install.rs` (no per-skill READMEs). |
| `BUNDLED_SKILLS` (multi-backend) | `crates/clud-bin/src/skills.rs:32` | Compile-time list paired with `include_str!("../assets/skills/<name>/SKILL.md")`. |
| `BUNDLED_SKILLS` (Claude-only) | `crates/clud-bin/src/skill_install.rs:32` | Same name, different content — paired with `include_str!("../../../skills/<name>/SKILL.md")`. |
| Launch dispatch | `crates/clud-bin/src/main.rs:36-44` | Calls both installers in sequence at startup. |

## Why two installers

The two installers are an interim historical reality, not a designed-in
split. `skill_install.rs` predates `skills.rs` (introduced in PR #88 for the
single `/clud-pr` skill) and was kept in place when the broader multi-backend
expander landed. The two flows now serve overlapping but distinct purposes:

| Concern | `skills.rs` | `skill_install.rs` |
|---|---|---|
| Backends targeted | Every entry in `SKILL_BACKENDS` whose home subdir exists (Claude + Codex today; Codex is gated on `~/.codex` and writes to `~/.agents/skills`) | Hard-coded `~/.claude/skills/` only |
| Source tree | `crates/clud-bin/assets/skills/` | Top-level `skills/` |
| Behavior on existing file | Skip — user edits are preserved | Compare modulo whitespace; overwrite when semantically divergent |
| Bundled skill set | `clud-issue`, `clud-issue-triage`, `clud-pr`, `clud-tag-release` | `clud-pr`, `clud-pr-merge`, `clud-issue` |
| Error policy | Non-fatal; one `[clud] note: ...` on failure | Non-fatal; one `[clud] note: ...` on failure |

Both invariants are enforced by tests in their respective modules: see
`skips_existing_and_preserves_user_edits` in `skills.rs:225` and
`semantic_diff_overwrites_with_embedded_copy` in `skill_install.rs:258`.

## Install-on-launch flow

`main()` runs both installers near the top of `clud` startup, after the
trampoline unlock and the console-title stamp but before argument parsing.
The order (see `crates/clud-bin/src/main.rs:36-44`):

1. `skills::ensure_installed()` — multi-backend, preserve-existing pass.
   It first purges stale clud-managed Codex copies from `~/.codex/skills/`,
   then returns a per-backend `InstallReport` listing what was installed vs
   skipped in the active target directories. Errors only when
   `$HOME`/`$USERPROFILE` cannot be resolved or active-target installs fail.
2. `skill_install::ensure_installed()` — Claude-only drift pass. No return
   value; logs `[clud] installed /<name>` on first install and
   `[clud] updated /<name>` (green) on a semantic overwrite.

Both are wrapped so a failure logs a `[clud] note: ...` line on stderr and
launch continues. A skills hiccup must never block the backend from
starting — see the "best-effort startup nudges" entry in
`crates/clud-bin/src/README.md`.

## Bundling mechanism

Each `SKILL.md` is pulled into the binary at compile time with
`include_str!`. There is no `include_dir` crate in `Cargo.toml` and no
runtime filesystem lookup of source content — the embedded string is the
single source of truth. Consequences:

- Adding a `SKILL.md` to the source tree without registering it in the
  matching `BUNDLED_SKILLS` constant does nothing at runtime.
- A typo in the `include_str!` path is a build-time error.
- A zero-byte source file compiles cleanly but ships an empty skill — the
  `bundled_skills_are_non_empty` test in `skills.rs:263` and
  `every_bundled_skill_has_real_content` in `skill_install.rs:296` guard
  against that.

## Source-tree divergence

Two source trees back the two installers, and their contents do not match:

| Skill | `assets/skills/` (`skills.rs`) | top-level `skills/` (`skill_install.rs`) |
|---|---|---|
| `clud-issue` | yes | yes |
| `clud-pr` | yes | yes |
| `clud-issue-triage` | yes | no |
| `clud-tag-release` | yes | no |
| `clud-pr-merge` | no | yes |

Practical consequences:

- `clud-pr-merge` is Claude-only — Codex users never get it.
- `clud-issue-triage` and `clud-tag-release` are installed only via the
  preserve-existing path, so once written they are never refreshed by the
  drift installer.
- For `clud-issue` and `clud-pr`, the two source files can drift apart in
  the repo. The drift installer treats the top-level `skills/<name>/SKILL.md`
  as canonical for Claude and will overwrite a Claude install whose content
  matches the `assets/skills/` copy but not the top-level one.

The `assets/skills/` README flags this explicitly as a future-consolidation
target — see `crates/clud-bin/assets/skills/README.md:14-19`.

## Adding a skill

Use this checklist:

1. **Decide which installer(s) the skill ships through.** Multi-backend
   coverage requires `skills.rs`; drift-on-divergence semantics require
   `skill_install.rs`. Most new skills want `skills.rs` only.
2. **Drop the source file.**
   - For `skills.rs`: `crates/clud-bin/assets/skills/<name>/SKILL.md`
     with standard frontmatter (`name:`, `description:`, `triggers:`) and
     the `<!-- managed-by: clud -->` marker. Add a sibling `README.md`
     describing the skill for contributors.
   - For `skill_install.rs`: `skills/<name>/SKILL.md` at the repo root.
     The frontmatter must include `name: <name>` — the
     `every_bundled_skill_has_real_content` test asserts it.
3. **Register the entry.** Append a `BundledSkill { name, skill_md:
   include_str!("...") }` to the relevant `BUNDLED_SKILLS` constant
   (`skills.rs:32` or `skill_install.rs:32`). The two constants share a
   name but have different field shapes (`skill_md` vs `content`) and live
   in different modules.
4. **Link the README** from the **Skills** section of
   `crates/clud-bin/assets/skills/README.md` if applicable.
5. **Run `bash lint` and `bash test`.** The bundle tests assert non-empty
   content, unique names, and the `managed-by: clud` marker.

## Key types / constants

- `BundledSkill` (`skills.rs:25`): public struct with `name` and `skill_md`
  fields; used by `install_to` in tests and by external callers.
- `Skill` (`skill_install.rs:25`): private struct with `name` and `content`
  fields; module-internal only.
- `BUNDLED_SKILLS` (both modules): same identifier in both files,
  different element type, different source-tree backing. **This collision
  is the single biggest footgun when navigating between the two installers
  — always check which module you are in.**
- `SKILL_BACKENDS` (`skills.rs:83`): the list of backend install gates and
  skill target directories the multi-backend installer iterates. Codex uses
  `.codex` as the installed-backend gate and `.agents/skills` as the current
  skill target. Adding a new CLI is a one-line append; see the commented
  OpenRouter example.
- `ensure_installed` (both modules): the public launch-time entry point;
  same name, different return type (`Result<Vec<...>, InstallError>` vs
  `()`).
- `normalize` (`skill_install.rs:96`): the whitespace-tolerant equality
  helper that makes CRLF-vs-LF a no-op on Windows checkouts.
- `Existing` (`skill_install.rs:71`): four-variant state classifier
  (`Missing` / `Matches` / `Diverges` / `Unreadable`) that drives the
  drift-installer's action choice.
- `InstallReport` (`skills.rs:127`): per-backend result of an install
  pass, listing `installed` and `skipped_existing` skills by name.
- `LegacyPurgeReport` (`skills.rs:127`): best-effort cleanup summary for
  clud-managed `~/.codex/skills/<name>/SKILL.md` copies removed during the
  migration away from the legacy Codex skill directory.

## Consolidation plan (open)

The dual installer is acknowledged interim state. The
`assets/skills/README.md` notes the redundancy and the per-skill READMEs
document which installer each skill ships through. Future work should
collapse this into a single installer with a single source tree; the
resolution (which behavior wins on existing files, which source tree
survives, how to keep Codex coverage while preserving drift detection for
Claude) is not prescribed here.

## See also

- `../../crates/clud-bin/assets/skills/README.md` — bundled-skill index and
  the original dual-installer caveat.
- `../DESIGN_DECISIONS.md` (DD-008) — design rationale for the skill-system
  shape.
