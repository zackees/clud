# dylints/

Custom Dylint lints for clud. Dylint crates are excluded from the stable
workspace build because they use rustc internals and pin their own nightly.

- `ban_manual_slash_normalize` bans hand-rolled `.replace('\\', "/")` path
  separator rewrites and directs callers to `clud::path_norm`.

Run locally:

```bash
rustup toolchain install nightly-2026-03-26 --component llvm-tools-preview --component rust-src --component rustc-dev --profile minimal
soldr --no-cache cargo install cargo-dylint dylint-link --version 5.0.0 --locked
uv run --no-project python ci/build_dylint_driver.py
ZCCACHE_DISABLE=1 soldr --no-cache cargo +nightly-2026-03-26 dylint --all -- --workspace --all-targets
```
