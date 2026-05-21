# clud-tag-release/

Bundled skill that drives a tag-triggered release for Rust and Python projects. Activates on `/clud-tag-release` (with or without a version arg) or natural-language phrases like "cut a release" or "tag a release". It detects the workspace version from `Cargo.toml` or `pyproject.toml`, runs pre-flight gates (clean default branch, no duplicate tag, green CI, an auto-release workflow that listens for version tags), pushes an annotated tag to origin, and surfaces the URL of the triggered workflow run.

## Files

- `SKILL.md` — Skill frontmatter, triggers, the five hard rules, the step-by-step workflow, and failure modes to avoid.

## How it ships

The `SKILL.md` body is embedded into the `clud` binary at compile time via `include_str!` from `crates/clud-bin/src/skills.rs` (see the `BUNDLED_SKILLS` array) and again from `crates/clud-bin/src/skill_install.rs`. On every launch, `skill_install::ensure_installed` writes the embedded copy to `~/.claude/skills/clud-tag-release/SKILL.md` if it's missing and overwrites it if the on-disk copy diverges semantically (whitespace-only diffs are ignored). `skills::ensure_installed` mirrors the same payload into every detected backend (`~/.claude/skills/`, `~/.codex/skills/`) whose home subdir already exists, never overwriting an existing file. Install failures are non-fatal: they log a `[clud] note: ...` line and launch proceeds.
