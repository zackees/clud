# dylints/

Custom Dylint lints for clud. Dylint crates are excluded from the stable
workspace build because they use rustc internals and pin their own nightly.

- `ban_manual_slash_normalize` bans hand-rolled `.replace('\\', "/")` path
  separator rewrites and directs callers to `clud::path_norm`.

Run locally:

```bash
soldr rustup toolchain install nightly-2026-05-28 --component llvm-tools-preview --component rust-src --component rustc-dev --profile minimal
SOLDR_FORCE_MANAGED_CARGO_SUBCOMMANDS=1 RUSTUP_TOOLCHAIN=nightly-2026-05-28 soldr --no-cache cargo dylint --all -- --workspace --all-targets
```

Soldr 0.9.10's blessed 6.0.3 Dylint lane supplies precompiled `cargo-dylint`
and `dylint-link` binaries for the host platform. Keep `dylint_linting` at
**6.0.3** with those tools, and keep the nightly above matched to
`ban_manual_slash_normalize/rust-toolchain`. `tests/test_dylint_stack.py`
asserts that lockstep.

`SOLDR_FORCE_MANAGED_CARGO_SUBCOMMANDS=1` is also intentional. It prevents a
previously installed `cargo-dylint` or `dylint-link` on `PATH` from replacing
Soldr's matched 6.0.3 tool set.

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
