# Documentation

Architecture and design records for clud. Read whichever doc matches what you're working on; nothing here needs to be read in order.

- **[ARCHITECTURE.md](ARCHITECTURE.md)** — index of subsystem topic docs (loop, daemon IPC, session lifecycle, skill system, gc/registry, Windows quirks, launch plan).
- **[DESIGN_DECISIONS.md](DESIGN_DECISIONS.md)** — ADR-style records for non-obvious design choices.
- **[architecture/](architecture/)** — the subsystem docs themselves.

Per-directory `README.md` files under `crates/` and `testbins/` describe **what's in this directory**. The docs here describe **how subsystems work across directories**.

See the root [`CLAUDE.md`](../CLAUDE.md) for build / lint / test commands and the rule for where new docs go.
