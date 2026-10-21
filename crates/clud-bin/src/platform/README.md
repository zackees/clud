# platform/

The host-platform boundary. This is the only place in `clud-bin`'s sources
where a host may be named. Rules and rationale:
[`docs/architecture/platform-boundary.md`](../../../../docs/architecture/platform-boundary.md).

- `mod.rs` is the selector. It declares `unix` or `windows` and re-exports
  the selected tree, so `crate::platform::<area>` resolves to the current
  host's implementation.
- `windows/` and `unix/` hold one module per area with the same public
  items in both trees: for example `host` (the `WINDOWS`/`UNIX`/`LINUX`/`MACOS`
  consts) and `subprocess` (the suspended spawn and the `.cmd`/`.bat`
  rewrite). Each tree's `mod.rs` lists its areas.
- When a concept exists on one host only, the other tree's module is a stub
  that preserves that host's behaviour.

To add an area, create `windows/<area>.rs` and `unix/<area>.rs` with matching
signatures, declare `pub mod <area>;` in both `mod.rs` files, and call
`crate::platform::<area>::..` from the neutral module.

## Kernel fixtures

`unix/process_identity_zombie_tests.rs` contains the Linux-only `waitid(WNOWAIT)`
regression for zombie liveness (#1993). `unix/mod.rs` includes it as
a test module so the native API and host selection stay inside this boundary.
The raw child remains unreaped during direct and batched identity observations,
then is reaped before the assertions. No production wait API is introduced.
