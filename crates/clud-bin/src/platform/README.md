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
