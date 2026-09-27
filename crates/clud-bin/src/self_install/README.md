# Native self-install catalog

`entry.rs` owns the early CLI and first-run gates. Explicit `--installer`
dispatch runs before the runtime-cache hop, Windows trampoline, daemon,
backend, title keeper, and launch setup. The automatic offer is limited to a
bare, interactive `clud` invocation when no executable named `clud` resolves
on the effective PATH. `picker.rs` supplies the menu, confirmation, and full
release-list models to the shared `selector.rs` terminal driver. The automatic
menu defaults to **Not now**; the explicit menu defaults to **Install this
version**. All install intents are read-only until the transaction engine is
added. Accepting an install currently returns an explicit failure.

`catalog.rs` parses the bounded, duplicate-key rejecting v1 Catalog published
at the Pages installer endpoint. `Catalog::compatible_releases` lists complete
versions for a host, and `Catalog::resolve` chooses an exact version or the
`latest-stable` channel. Linux always prefers the static musl variant. A GNU
asset is eligible only after the caller supplies a verified non-NixOS GNU
loader and glibc 2.17+ result through `Host::gnu`.

`ResolvedAsset::verify_bytes` checks the catalog size and SHA-256 before
installation. The selected musl asset must also be a matching ELF64 binary
without an interpreter or needed shared library. A failed selected asset is
an error; callers must not try another catalog variant after verification.

The host probe in `entry.rs` verifies the non-NixOS GNU loader and glibc floor
before it grants GNU eligibility. The published catalog generator remains in
`installer/catalog.py`; this module is the native reader of its output.
