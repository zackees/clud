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
