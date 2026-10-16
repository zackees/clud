# Platform boundary

clud names a host platform in exactly one kind of place: a **platform
boundary**. Everywhere else, the code is neutral and calls a host-independent
API. The `enforce_platform_boundary` Dylint lint enforces this with no
baseline and no allowlist (#1946, #1953;
[DD-170](../DESIGN_DECISIONS.md#dd-170-the-platform-boundary-has-no-exceptions)).
soldr, zccache and fbuild enforce the same boundary.

## What counts as naming a host

- A host key in `#[cfg(..)]`, `#![cfg(..)]`, the condition of
  `#[cfg_attr(..)]`, `cfg!`, `cfg_if!` or `cfg_select!`. That includes keys
  nested in `any`/`all`/`not` and inside other macros' tokens. The keys are
  `windows`, `unix`, `target_os`, `target_family`, `target_arch`,
  `target_env`, `target_abi`, `target_endian`, `target_feature`,
  `target_pointer_width` and `target_vendor`.
- A path into a native API crate: `libc`, `windows_sys`, `windows`, `winapi`,
  `nix` or `mach2`.

`cfg(test)`, `cfg(feature = ..)` and `cfg(debug_assertions)` are not host keys.

The pass runs **before cfg expansion**, so it sees inactive branches. The one
Linux Dylint run therefore checks the Windows and macOS code, inline tests,
and macro tokens as well.

## Where a host may be named

A crate's boundary is `src/platform/`, directly under `src`:

```
src/platform/mod.rs        the only selector: cfg'd `mod` + `pub use`
src/platform/windows/      concrete Windows tree
src/platform/unix/         concrete Unix tree (Linux and macOS; may use
                           cfg(target_os) internally)
src/platform/linux/, macos/  reserved for trees selected separately
```

A test suite may hold a boundary at any depth under `tests/`, such as
`crates/clud-bin/tests/integration/reaper/platform/`. That keeps a category's
host-specific cases beside its neutral ones. Any other file in a `platform/`
directory is a facade and must stay neutral. A `platform/` directory deeper
inside `src` is not a boundary.

## How `crates/clud-bin` uses it

`platform/mod.rs` re-exports the selected tree, so an area module declared in
both `platform/windows/mod.rs` and `platform/unix/mod.rs` is reached as
`crate::platform::<area>`. Both trees expose the same public items with the
same signatures. CI type-checks Linux, Windows and macOS, so drift fails one
of them.

If a concept exists on one host only, the other tree keeps the old behaviour
there with a stub: a no-op, `false`/`None`, an `io::ErrorKind::Unsupported`
error, or an uninhabited `pub enum X {}` whose methods are `match *self {}`.

`crate::platform::host::{WINDOWS, UNIX, LINUX, MACOS}` replace `cfg!(..)` in
expressions.

Host-specific tests live in a `#[cfg(test)] mod tests` inside the concrete
file they exercise. A test that only expects different results per host
branches on `host::*` at run time.

Other crates (`crates/tap`, the `testbins`) and the integration-test
categories each carry their own small boundary in the same shape.
