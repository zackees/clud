# dylints/

Custom Dylint lints for clud. Dylint crates are excluded from the stable
workspace build because they use rustc internals and pin their own nightly.

- `ban_manual_slash_normalize` bans hand-rolled `.replace('\\', "/")` path
  separator rewrites and directs callers to `clud::path_norm`.

Run locally:

```bash
export SOLDR_DYLINT_TOOLCHAIN=nightly-2026-05-28
env -u RUSTUP_TOOLCHAIN soldr dylint prepare
env -u RUSTUP_TOOLCHAIN soldr dylint --all -- --workspace --all-targets
```

Soldr 0.9.23's verified 6.0.3 Dylint lane supplies precompiled `cargo-dylint`,
`dylint-link`, and the matching nightly driver for each supported host. Keep `dylint_linting` at
**6.0.3** with those tools, and keep the nightly above matched to
`ban_manual_slash_normalize/rust-toolchain`. `tests/test_dylint_stack.py`
asserts that lockstep.

Clearing an inherited `RUSTUP_TOOLCHAIN` for `soldr dylint prepare` is
intentional: CI's stable Rust selector must not override the lint crate's
nightly pin. Set `SOLDR_DYLINT_TOOLCHAIN=nightly-2026-05-28` for these commands:
Soldr's preparer reads `rust-toolchain.toml`, while this crate intentionally
retains Dylint's bare `rust-toolchain` file. Soldr then selects the matching
verified tool, linker, and driver set.

The legacy bare `rust-toolchain` filename is intentional. Dylint 6.0.3 unsets
`RUSTUP_TOOLCHAIN` while building its driver and recognizes this filename when
selecting the lint crate's nightly. Renaming it to `rust-toolchain.toml` makes
6.0.3 fall back to stable and fail; upgrading to 6.0.4 only to recognize that
newer filename would also give up Soldr's blessed precompiled fast path.

`.cargo/config.toml` in the lint crate is load-bearing, not boilerplate. It
routes linking through `dylint-link`, the wrapper that names the cdylib
`lib<name>@<toolchain>.so` -- the exact filename Dylint looks up after building.
Without it the build succeeds and emits a plain `lib<name>.so`, and Dylint
fails with "Could not find ... despite successful build".

That was the real cause of the failure CI used to work around by copying the
artifact to the suffixed name and re-running Dylint. It read as an upstream
artifact-naming gap; it was a missing linker config here, absent because this
crate was not created from Dylint's template. The linker config and the legacy
toolchain filename are both required for the blessed 6.0.3 path.

If the missing-alias failure ever recurs, check `.cargo/config.toml` before
suspecting Dylint.
