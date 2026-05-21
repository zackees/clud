# clud-bin

Main Rust binary crate for the `clud` project. Builds the `clud` executable
(a fast CLI for running Claude Code and Codex in YOLO mode) and is distributed
as a Python wheel via maturin with `bindings = "bin"` — installing the wheel
drops the native binary onto `PATH`.

The crate also exposes a `clud` library target (`src/lib.rs`) so integration
tests can exercise internals directly.

## Layout

- [`src/`](src/README.md) — binary entry point, library modules, platform code
- [`tests/`](tests/README.md) — integration tests (PTY behavior, etc.)
- [`assets/`](assets/README.md) — bundled non-code resources
- `Cargo.toml` — crate manifest (package `clud`, binary `clud`, library `clud`)

## Build

From the repo root:

```bash
bash build                       # dev wheel (Rust binary + Python package)
soldr cargo build -p clud-bin    # bare Rust binary only
```

All `cargo` / `rustc` / `rustfmt` invocations must go through
[`soldr`](https://github.com/zackees/soldr) — see the project `CLAUDE.md`.

## Test

```bash
bash test                        # Rust unit tests + Python unit tests
bash test --integration          # adds end-to-end tests with mock agents
soldr cargo test -p clud-bin     # crate-level Rust tests only
```

## Key deps

- `clap` (derive) — CLI argument parsing with passthrough for unknown flags
- `crossterm` / `vt100` / `vte` — terminal I/O and PTY stream parsing
- `redb` — embedded KV store for session and tracked-entry registries
  (issues #73 / #110; replaced bundled `rusqlite`)
- `fs4` — cross-platform advisory file lock serializing concurrent redb opens
  (issue #138)
- `windows` / `windows-core` — Win32 console drag-and-drop (`IDropTarget`)
  and related shell integration (issue #79)
- `cpal` + `rodio` (non-Linux) and `whisper-rs` (off on Windows ARM) —
  voice-mode capture and transcription (issue #13); Linux capture shells
  out to `arecord` to keep libasound off the hot path
- `ureq` + `sha2` + `dirs` — sync model auto-downloader with hash verify
- `ignore` — gitignore-aware repo walk for the large-file startup guard
  (issue #132)

## Distribution

Packaged as a Python wheel via maturin (`[tool.maturin] bindings = "bin"`,
`manifest-path = "crates/clud-bin/Cargo.toml"` in the root `pyproject.toml`).
The wheel installs the `clud` binary onto the user's `PATH` — no Python
runtime code ships beyond a thin version shim.

CI builds across 6 platforms: Linux x86 + ARM, Windows x86 + ARM, macOS
ARM + x86. The Windows ARM target stubs out `whisper-rs` because the
vendored C++ source does not compile on `aarch64-pc-windows-msvc`.
