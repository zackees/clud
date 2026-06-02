# assets/

Compile-time-embedded resources bundled directly into the `clud-bin` binary. Files here are read by `include_str!` during the Rust build, so the shipped binary carries them in its read-only data segment and never touches the filesystem to locate them at runtime.

## Layout

- [`skills/`](skills/README.md) — slash-command skill definitions (`SKILL.md` files) installed per-backend into `.claude/skills/` or Codex's current `.agents/skills/` path on first run.

## Embedding mechanism

Each asset is referenced from Rust source via `include_str!("../assets/...")` in [`src/skills.rs`](../src/skills.rs) (and a parallel set in [`src/skill_install.rs`](../src/skill_install.rs)). The macro inlines the file contents at compile time, so adding, removing, or modifying an asset requires a rebuild. There is no runtime lookup, no packaging step beyond `cargo build`, and no dependency on the install location of the binary.
