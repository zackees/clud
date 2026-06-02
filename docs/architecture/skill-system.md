# Skill System

Skills are slash-command playbooks (`/clud-pr`, `/clud-issue`, etc.) that ship
embedded inside the `clud` binary as compile-time string assets and are
installed into the user's backend skill directories (`~/.claude/skills/` and
Codex's current `~/.agents/skills/`) only during global launch setup. Regular
session-only launches do not write persistent agent setup files. Bare
interactive launches can opt into global setup with the selector in
`launch_setup.rs`; automation, piped stdin, `--dry-run`, and one-shot prompt
launches default to session-only. Older clud-managed Codex copies in
`~/.codex/skills/` are purged best-effort only when Codex global setup runs.

## Component map

| Component | Path | Role |
|---|---|---|
| Multi-backend installer | `crates/clud-bin/src/skills.rs` | Iterates `BUNDLED_SKILLS` and `SKILL_BACKENDS`; writes only when the target file is missing. |
| Claude-only drift installer | `crates/clud-bin/src/skill_install.rs` | Reads from top-level `skills/`; overwrites on semantic divergence. |
| Multi-backend source tree | `crates/clud-bin/assets/skills/<name>/` | Holds `SKILL.md` + `README.md` for skills consumed by `skills.rs`. |
| Top-level source tree | `skills/<name>/SKILL.md` | Holds skills consumed by `skill_install.rs` (no per-skill READMEs). |
| `BUNDLED_SKILLS` (multi-backend) | `crates/clud-bin/src/skills.rs:32` | Compile-time list paired with `include_str!("../assets/skills/<name>/SKILL.md")`. |
| `BUNDLED_SKILLS` (Claude-only) | `crates/clud-bin/src/skill_install.rs:32` | Same name, different content; paired with `include_str!("../../../skills/<name>/SKILL.md")`. |
| Launch setup gate | `crates/clud-bin/src/launch_setup.rs` | Selects session-only vs global setup and dispatches selected-backend setup actions. |

## Why Two Installers

The two installers are an interim historical reality, not a designed-in split.
`skill_install.rs` predates `skills.rs` and was kept in place when the broader
multi-backend expander landed. The two flows now serve overlapping but distinct
purposes:

| Concern | `skills.rs` | `skill_install.rs` |
|---|---|---|
| Backends targeted | The selected backend during global setup (Claude + Codex today; Codex is gated on `~/.codex` and writes to `~/.agents/skills`) | Hard-coded `~/.claude/skills/` only; run only for Claude global setup |
| Source tree | `crates/clud-bin/assets/skills/` | Top-level `skills/` |
| Behavior on existing file | Skip; user edits are preserved | Compare modulo whitespace; overwrite when semantically divergent |
| Bundled skill set | `clud-issue`, `clud-issue-triage`, `clud-pr`, `clud-tag-release` | `clud-pr`, `clud-pr-merge`, `clud-issue` |
| Error policy | Non-fatal; one `[clud] note: ...` on failure | Non-fatal; one `[clud] note: ...` on failure |

Both invariants are enforced by tests in their respective modules: see
`skips_existing_and_preserves_user_edits` in `skills.rs` and
`semantic_diff_overwrites_with_embedded_copy` in `skill_install.rs`.

## Global Setup Flow

`main()` resolves the backend first, then asks `launch_setup.rs` for a setup
scope before building the final `LaunchPlan`. Non-prompting launches receive
`SessionOnly` and skip all persistent setup actions. Bare interactive launches
show this selector on stderr:

```text
[x] Session only
[ ] Globally
```

Enter accepts the highlighted option. Up selects session-only; Down selects
global. When the selected scope is global, `launch_setup::run_setup` runs only
the actions registered for the selected backend:

1. `skills::ensure_installed_for_backend()` - multi-backend,
   preserve-existing pass for the selected backend. Codex global setup first
   purges stale clud-managed Codex copies from `~/.codex/skills/`, then writes
   bundled skills to `~/.agents/skills/` when `~/.codex` exists.
2. `skill_install::ensure_installed()` - Claude-only drift pass. It runs only
   when the selected backend is Claude and the scope is global. No return
   value; logs `[clud] installed /<name>` on first install and
   `[clud] updated /<name>` on a semantic overwrite.

Both are wrapped so a failure logs a `[clud] note: ...` line on stderr and
launch continues. A skills hiccup must never block the backend from starting.
See [launch-setup.md](launch-setup.md) for the selector contract and the other
global setup actions.

## Bundling Mechanism

Each `SKILL.md` is pulled into the binary at compile time with `include_str!`.
There is no runtime filesystem lookup of source content. Consequences:

- Adding a `SKILL.md` to the source tree without registering it in the matching
  `BUNDLED_SKILLS` constant does nothing at runtime.
- A typo in the `include_str!` path is a build-time error.
- A zero-byte source file compiles cleanly but ships an empty skill. The
  bundled-skill tests guard against that.

## Source-Tree Divergence

Two source trees back the two installers, and their contents do not match:

| Skill | `assets/skills/` (`skills.rs`) | top-level `skills/` (`skill_install.rs`) |
|---|---|---|
| `clud-issue` | yes | yes |
| `clud-pr` | yes | yes |
| `clud-issue-triage` | yes | no |
| `clud-tag-release` | yes | no |
| `clud-pr-merge` | no | yes |

Practical consequences:

- `clud-pr-merge` is Claude-only; Codex users never get it.
- `clud-issue-triage` and `clud-tag-release` are installed only via the
  preserve-existing path, so once written they are never refreshed by the drift
  installer.
- For `clud-issue` and `clud-pr`, the two source files can drift apart in the
  repo. The drift installer treats the top-level `skills/<name>/SKILL.md` as
  canonical for Claude and will overwrite a Claude install whose content
  matches the `assets/skills/` copy but not the top-level one.

## Adding a Skill

Use this checklist:

1. Decide which installer(s) the skill ships through. Multi-backend coverage
   requires `skills.rs`; drift-on-divergence semantics require
   `skill_install.rs`. Most new skills want `skills.rs` only.
2. Drop the source file.
   - For `skills.rs`: `crates/clud-bin/assets/skills/<name>/SKILL.md` with
     standard frontmatter (`name:`, `description:`, `triggers:`) and the
     `<!-- managed-by: clud -->` marker. Add a sibling `README.md` describing
     the skill for contributors.
   - For `skill_install.rs`: `skills/<name>/SKILL.md` at the repo root. The
     frontmatter must include `name: <name>`.
3. Register the entry. Append a `BundledSkill { name, skill_md:
   include_str!("...") }` to the relevant `BUNDLED_SKILLS` constant
   (`skills.rs` or `skill_install.rs`). The two constants share a name but have
   different field shapes (`skill_md` vs `content`) and live in different
   modules.
4. Link the README from the Skills section of
   `crates/clud-bin/assets/skills/README.md` if applicable.
5. Run `bash lint` and `bash test`. The bundle tests assert non-empty content,
   unique names, and the `managed-by: clud` marker.

## Key Types / Constants

- `BundledSkill` (`skills.rs`): public struct with `name` and `skill_md`
  fields; used by `install_to` in tests and by external callers.
- `Skill` (`skill_install.rs`): private struct with `name` and `content`
  fields; module-internal only.
- `BUNDLED_SKILLS` (both modules): same identifier in both files, different
  element type, different source-tree backing.
- `SKILL_BACKENDS` (`skills.rs`): the list of backend install gates and skill
  target directories the multi-backend installer can target. Codex uses
  `.codex` as the installed-backend gate and `.agents/skills` as the current
  skill target.
- `ensure_installed_for_backend` (`skills.rs`): the global-setup entry point
  used by `launch_setup.rs` for the selected backend.
- `ensure_installed` (both modules): compatibility helpers; same name,
  different return type (`Result<Vec<...>, InstallError>` vs `()`), but
  `main.rs` no longer calls them unconditionally on every launch.
- `normalize` (`skill_install.rs`): the whitespace-tolerant equality helper
  that makes CRLF-vs-LF a no-op on Windows checkouts.
- `Existing` (`skill_install.rs`): four-variant state classifier
  (`Missing` / `Matches` / `Diverges` / `Unreadable`) that drives the
  drift-installer's action choice.
- `InstallReport` (`skills.rs`): per-backend result of an install pass,
  listing `installed` and `skipped_existing` skills by name.
- `LegacyPurgeReport` (`skills.rs`): best-effort cleanup summary for
  clud-managed `~/.codex/skills/<name>/SKILL.md` copies removed during Codex
  global setup after the migration away from the legacy Codex skill directory.

## Consolidation Plan

The dual installer is acknowledged interim state. The `assets/skills/README.md`
notes the redundancy and the per-skill READMEs document which installer each
skill ships through. Future work should collapse this into a single installer
with a single source tree; the resolution is not prescribed here. The current
installers remain behind the launch setup scope gate so session-only launches
stay side-effect free.

## See Also

- `../../crates/clud-bin/assets/skills/README.md` - bundled-skill index and
  the original dual-installer caveat.
- `launch-setup.md` - selector and persistent setup action contract.
- `../DESIGN_DECISIONS.md` (DD-008) - design rationale for the skill-system
  shape.
