# dylints/

Custom Dylint lints for clud. Dylint crates are excluded from the stable
workspace build because they use rustc internals and pin their own nightly.

- `ban_manual_slash_normalize` bans hand-rolled `.replace('\\', "/")` path
  separator rewrites and directs callers to `clud::path_norm`.

Run locally:

```bash
rustup toolchain install nightly-2026-04-16 --component llvm-tools-preview --component rust-src --component rustc-dev --profile minimal
soldr --no-cache cargo install cargo-dylint dylint-link --version 6.0.1 --locked
RUSTUP_TOOLCHAIN=nightly-2026-04-16 ZCCACHE_DISABLE=1 soldr --no-cache cargo dylint --all -- --workspace --all-targets
```

Dylint 6.0.1 still has one upstream artifact-naming gap: on Ubuntu and
Windows it can build the lint cdylib without creating the toolchain-suffixed
alias that `cargo dylint` immediately looks up. The CI workflow has one narrow
recovery for that reproduced missing-alias shape, then reruns Dylint once. It
does not build or override a custom driver.
