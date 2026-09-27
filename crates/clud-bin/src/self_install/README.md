# Native self-install catalog

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

The native install transaction and host probe will call this module. The
published catalog generator remains in `installer/catalog.py`; this module is
the native reader of its output.
