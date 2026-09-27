# Native self-install catalog

`entry.rs` owns the early CLI and first-run gates. Explicit `--installer`
dispatch runs before the runtime-cache hop, Windows trampoline, daemon,
backend, title keeper, and launch setup. The automatic offer is limited to a
bare, interactive `clud` invocation when no executable named `clud` resolves
on the effective PATH. `picker.rs` supplies the menu, confirmation, and full
release-list models to the shared `selector.rs` terminal driver. The automatic
menu defaults to **Not now**; the explicit menu defaults to **Install this
version**. `transaction.rs` builds the read-only plan, verifies the selected
source, and commits one executable. `activation.rs` builds the read-only
shell plan shown before consent, updates owned Bash, zsh, or fish startup
files, and proves fresh login and interactive name-based lookup.
`activation_windows.rs` owns the HKCU User Path edit, change broadcast, and
OS-built user-environment proof on Windows. A binary commit alone does not
count as installer success.

`catalog.rs` parses the bounded, duplicate-key rejecting v1 Catalog published
at the Pages installer endpoint. `Catalog::compatible_releases` lists complete
versions for a host, and `Catalog::resolve` chooses an exact version or the
`latest-stable` channel. Linux always prefers the static musl variant. A GNU
asset is eligible only after the caller supplies a verified non-NixOS GNU
loader and glibc 2.17+ result through `Host::gnu`.
`Catalog::parse_candidate` admits one complete newer candidate row only when
an explicit fixed-origin release tag selects the versioned public candidate
catalog; ordinary parsing rejects its candidate channel.

`ResolvedAsset::verify_bytes` checks the catalog size and SHA-256 before
installation. The selected musl asset must also be a matching ELF64 binary
without an interpreter or needed shared library. A failed selected asset is
an error; callers must not try another catalog variant after verification.

The host probe in `entry.rs` verifies the non-NixOS GNU loader and glibc floor
before it grants GNU eligibility. The published catalog generator remains in
`installer/catalog.py`; this module is the native reader of its output.
Static musl builds use the canonical GNU loader and `/usr/bin/getconf
GNU_LIBC_VERSION` to probe a GNU host without linking glibc into the
candidate. The `installer-ci-fixture` feature is compiled only into dev PR
candidate builds; it reads local catalog and asset bytes while retaining the
normal parser, digest, extraction, staging, and activation paths.
