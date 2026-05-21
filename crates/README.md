# crates/

Holds the Rust crates that ship as part of the `clud` distribution. Each subdirectory is a Cargo crate with its own README covering build, layout, and design notes.

## Crates

- [`clud-bin/`](clud-bin/README.md) — main `clud` CLI binary, packaged as a Python wheel via maturin.

## Workspace

These crates are workspace members declared in the root [`Cargo.toml`](../Cargo.toml). Test-only binaries (e.g. mock agents used by the integration suite) live separately under [`testbins/`](../testbins/) and are not part of the shipped distribution.
