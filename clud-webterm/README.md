# clud-webterm — Phase-1 fidelity spike

Tracking issue: **zackees/clud#929**, Phase 1.

A throwaway spike that answers one question: **does a real `claude` / `codex`
TUI render and drive correctly inside `xterm.js` hosted in a Tauri webview?**
If yes, the whole "clud owns a web terminal" direction is green-lit.

## Stack

- **Tauri v2** — native webview shell (Rust backend).
- **`portable-pty`** — the PTY (ConPTY on Windows), spawns the shell/agent.
- **`xterm.js`** (vendored, offline, in `ui/vendor/`) — VT terminal emulator.

Deliberately **excluded from the root workspace** (see root `Cargo.toml`
`exclude`) so its heavy dep tree and process spawning don't touch the shipped
`clud` build, CI, or `bash lint`.

## Windows cross-build manifest

`windows-app-manifest.xml` is a Windows-only loader resource. Linux-hosted
Windows builds cannot run Tauri's usual host-side resource compiler, so
`build.rs` writes its compact `.res` representation directly and links it into
the Windows companion. It activates the operating system's Common Controls v6
implementation before Tauri resolves `TaskDialogIndirect`; it is not packaged
for Linux or macOS and does not add a DLL dependency.

## Run

```bash
# from repo root
soldr cargo run --manifest-path clud-webterm/Cargo.toml
```

A window opens running your platform shell (`cmd.exe` on Windows). To point it
straight at an agent, set `WEBTERM_CMD`:

```bash
WEBTERM_CMD=claude       soldr cargo run --manifest-path clud-webterm/Cargo.toml
WEBTERM_CMD="clud --help" soldr cargo run --manifest-path clud-webterm/Cargo.toml
```

The child is spawned with `CLUD_WEBTERM=1` in its env (the recursion guard the
real feature will check so a nested `clud` doesn't open another window).

## Fidelity checklist (what to eyeball)

Launch `claude` (or another full-screen TUI like `vim` / `htop`) inside the
window and confirm:

- [ ] Alternate screen buffer + full-screen redraw is correct
- [ ] 24-bit / 256 color renders right
- [ ] Cursor position, blinking, and shape behave
- [ ] Mouse reporting works (if the TUI uses it)
- [ ] Resizing the window reflows the TUI (SIGWINCH / ConPTY resize)
- [ ] `Ctrl-C`, `Ctrl-D`, arrow keys, and bracketed paste all pass through
- [ ] Heavy output (e.g. `cargo build`) stays responsive

## What this spike intentionally is NOT

Single tab, no settings, no toolbar, no error UI, no packaging. Tabs, the
`--web-term` launch wiring, and the toast/popup payoff are Phases 2–4 in #929.
