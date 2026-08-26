# dylints/

Custom Dylint lints for clud. Dylint crates are excluded from the stable
workspace build because they use rustc internals and pin their own nightly.

- `ban_manual_slash_normalize` bans hand-rolled `.replace('\\', "/")` path
  separator rewrites and directs callers to `clud::path_norm`.

Run locally:

```bash
rustup toolchain install nightly-2026-04-16 --component llvm-tools-preview --component rust-src --component rustc-dev --profile minimal
soldr cargo install cargo-dylint dylint-link --version 6.0.4 --locked
RUSTUP_TOOLCHAIN=nightly-2026-04-16 cargo-dylint dylint --all -- --workspace --all-targets
```

Invoke `cargo-dylint` directly for the final command. Soldr 0.9.10's managed
`cargo dylint` lane is pinned independently to 6.0.3 and prepares that older
driver before dispatch, while this lint crate requires 6.0.4.

Version pins (issue #911). `cargo-dylint`, `dylint-link` and `dylint_linting`
all move together at **6.0.4**, and the nightly above must match
`ban_manual_slash_normalize/rust-toolchain.toml`. `tests/test_dylint_stack.py`
asserts that lockstep, so bumping one site alone fails the suite.

`.cargo/config.toml` in the lint crate is load-bearing, not boilerplate. It
routes linking through `dylint-link`, the wrapper that names the cdylib
`lib<name>@<toolchain>.so` — the exact filename `cargo dylint` looks up after
building. Without it the build *succeeds* and emits a plain `lib<name>.so`,
and Dylint fails with "Could not find ... despite successful build".

That was the real cause of the failure CI used to work around by copying the
artifact to the suffixed name and re-running Dylint. It read as an upstream
artifact-naming gap in Dylint 6.0.1; it was a missing linker config here,
absent because this crate was not created from Dylint's template. Two independent things are required, established by three CI runs that
change one variable at a time (issue #911):

| Dylint | `.cargo/config.toml` | Result |
| --- | --- | --- |
| 6.0.4 | absent | fails: "Could not find ...@<toolchain>.so despite successful build" |
| 6.0.4 | present | **passes** |
| 6.0.3 | present | fails earlier, building Dylint's own driver |

So a version bump alone does not fix it, and neither does the linker config
alone. **6.0.4 is a floor, not just the newest release.** When Dylint builds
its driver it runs `cargo build` with `env -u RUSTUP_TOOLCHAIN`, then falls
back to the crate's rust-toolchain file to pick the toolchain. Only 6.0.4
learned to read the `rust-toolchain.toml` form, which is what this crate
uses; under 6.0.3 the driver builds against stable and fails. That is the
same breakage the retired `ci/build_dylint_driver.py` step was hand-patching.

The cost of 6.0.4 is real and worth knowing: its `dylint_linting` gained a
build-dependency that activates `clippy_utils` -> `git2`, so building this
lint crate now also compiles `libgit2-sys`/`libz-sys` and needs a C
toolchain. 6.0.3 avoids that but does not work with this layout. If you want
6.0.3 back, the lever is renaming `rust-toolchain.toml` to the legacy bare
`rust-toolchain` name — verify with a Dylint workflow dispatch before
trusting it.

If the missing-alias failure ever recurs, check `.cargo/config.toml` before
suspecting Dylint.
