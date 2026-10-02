---
name: terminal-pty-windows
description: Use before changing or debugging clud's terminal/PTY path on Windows (ConPTY, the native ReadConsoleInputW reader, VT input, raw mode, escape sequences, mouse/paste reports, Ctrl+C, terminal restore). Covers what ConPTY does differently, the files to read, the invariants, and how to validate without a local Windows machine.
---

# Terminal / PTY on Windows

Shared facts (data flow, the three invariants, test tiers, act rules, CI
labels) are in
[docs/architecture/terminal-pty.md](../../../docs/architecture/terminal-pty.md).
Read that first. This page lists only what differs on Windows.

## What is different on Windows

- **Input does not come from a byte stream.** On an interactive console,
  `console_input.rs` runs its own `ReadConsoleInputW` loop over
  running-process's translator. Each translated KEY_EVENT becomes one
  `extra_rx` chunk. The byte-stream stdin reader is not spawned.
- **VT input.** `console_setup::enable_console_vt_input` sets
  `ENABLE_VIRTUAL_TERMINAL_INPUT` for the session. It is layered on *after*
  the native reader has captured the original console mode
  (`runner_execution.rs:326` starts the reader, then `runner_execution.rs:349`
  takes the guard). Under VT input the console delivers terminal-originated
  sequences one KEY_EVENT per character: SGR mouse reports, focus events,
  bracketed-paste markers and DSR replies. The reader translates each event
  on its own, so a sequence reaches the pump in pieces. This is why
  invariant 1 (sequence atomicity, #1697) matters here.
- **Reader policies:**
  - The reader pairs UTF-16 surrogates itself (#1351).
  - Shift+Enter becomes `ESC CR`, because ConPTY rewrites LF to CR (#1369).
  - Ctrl+V can expand to a saved clipboard-image path.
  - Backspace `0x08` is normalized to `0x7f` (#1350).
- **Ctrl+C is a byte.** Raw mode clears `ENABLE_PROCESSED_INPUT`, so Ctrl+C
  arrives as `0x03` on `extra_rx`, and the pump treats it as an interrupt.
  No `CTRL_C_EVENT` fires.
- **ConPTY re-renders output.** On the ci-windows runner (#1709) we observed:
  - ConPTY **forwards** the child's alternate screen (`?1049`), bracketed
    paste (`?2004`) and cursor visibility (`?25`).
  - It does **not forward** the child's mouse modes (`?1003`/`?1006`), its
    scroll region, or cursor-key mode (`?1`).
  - ConPTY itself emits `?1004h` and `?9001h` (win32-input-mode) at startup.
- **ConPTY consumes focus reports on input.** A terminal's `ESC[I` /
  `ESC[O` never reaches the child as bytes, for every input path and
  chunking (observed on the x64 and arm runners, #1717). Everything else in
  `tests/pty/input_corpus.rs` must reach the child byte-for-byte; the only
  modelled transforms are LF→CR and this one (`expected_for`).
- **Cursor query at startup.** ConPTY sends `ESC[6n` and holds the child
  until a reply comes back (#1310). clud answers only when stdin is not an
  interactive console (`should_answer_cursor_queries`, `session.rs:1084`).
  Otherwise the real terminal answers.
- **Piped stdout breaks ConPTY.** ConPTY stops relaying child output when
  the spawning process's stdout is a pipe (#691). That is why CI runs the
  `pty` harness inside a pseudo-terminal (`ci/run_bundle.py:206`).
- **Resizing** goes through `resize_pty` (`session.rs:34`), which resizes the
  master directly, because `NativePtyProcess::resize_impl` is a no-op on
  Windows.

## Files to read first

- `crates/clud-bin/src/console_input.rs`: `spawn_console_input_reader`
  (`:81`), surrogate pairing, and the Shift+Enter / Ctrl+V policies.
- `crates/clud-bin/src/console_setup.rs`: `ConsoleVtGuard` (`:110`) and
  `enable_console_vt_input` (`:140`).
- `crates/clud-bin/src/runner_execution.rs:320-350`: reader and guard
  ordering.
- `crates/clud-bin/src/session.rs`:
  - `forward_user_input` (`:1252`)
  - `BracketedPasteNormalizer::flush_due_in` (`session/bracketed_paste.rs`):
    a held lone Esc is released after 5 ms, a partial report after 250 ms
    (#1717)
  - `should_answer_cursor_queries` (`:1084`)
  - `RawTerminalGuard` (`:316`, `Drop` at `:602`)
- `crates/clud-bin/src/session/escape_gate.rs`: `EscapeSequenceGate`.
- `crates/clud-bin/src/session/child_modes.rs`: what gets undone on exit.
- `crates/clud-bin/src/session/interrupt.rs:26`: `interrupt_pty_process`
  (daemon handoff, then tree kill, then PTY close).
- [windows-quirks.md](../../../docs/architecture/windows-quirks.md), sections
  (c) VT-input RAII, (d) native terminal input, and (h) `kill_tree`.
- [session-lifecycle.md](../../../docs/architecture/session-lifecycle.md),
  "The pump loop" and "Terminal restore".

## Invariants you must not break

1. **No partial escape sequence in a PTY write.** Every user-input write
   goes through `forward_user_input`, so it passes the gate. Do not add a
   side path that calls `write_impl` with keyboard bytes. Drag-drop chunks
   carry no ESC, so they are safe.
2. **Keep the native reader's ordering.** It must capture the original
   console mode before VT input is enabled. Dropping the guards restores VT
   input first, then the reader's original mode.
3. **Each scanner resumes across chunks.** `extra_rx` and stdin each have
   their own `InterruptScanner` (#1703).
4. **Do not stub a reply a real console will also send.** With an
   interactive console, a clud `ESC[6n` stub would be a second, wrong reply
   (#31).
5. **On restore, undo only what the child turned on.** Do not add a
   blanket `?1049l`.

## How to validate a change

There is no local Windows, and `act` cannot run Windows.

1. Run the shared Linux checks first: `bosn run --task act-ci-static` and
   `bosn run --task act-ci-linux`. See terminal-pty.md § Validation for the
   container gotchas.
2. Add the `ci-windows` label to the PR. It runs static checks plus the
   Windows x64 build (with clippy), unit and integration lanes.
3. For a bug fix, push a **tests-only commit first** so Windows records RED,
   then push the fix.
4. Watch the run with
   `clud tool run github/pr_merge_watch.py -- <PR> [--no-cancel]`. Read a
   failing job with
   `gh api --allow-escape-sequences repos/zackees/clud/actions/jobs/<job_id>/logs`.

Do not run `bash lint --windows` on the host. Agents must not run lint
outside act; rely on the clippy step in the CI Windows build.

Tests that need a real console call `require_pty_or_skip!`. They skip when
stdin is not a console, except under `CLUD_REQUIRE_PTY=1` (set by CI). To
feed real key events, inject KEY_EVENT records with `WriteConsoleInputW`, as
`crates/clud-bin/tests/pty/shift_enter_dual_reader.rs:62` does.

## Known behaviours and gotchas

- **#1697.** `extra_rx_mouse_reports_split_per_character_reach_the_child_whole`
  (`crates/clud-bin/tests/pty/pty_pump.rs:745`) reproduced the user's exact
  symptom on real ConPTY before the fix. The child read
  `35;31;18M\u{1b}[<35;31;19M…`. Use it as the template for any
  split-sequence regression.
- **#1704 / #1709.** `a_ctrl_c_interrupted_child_has_its_terminal_modes_undone`
  (`tests/pty/pty_pump.rs:378`) asserts only `?1049l`, `?2004l` and `?25h` on
  Windows, because ConPTY did not forward the others. Do not "fix" this test
  by asserting the POSIX set on Windows.
- **Windows-only compile error.** `#[cfg(windows)]` placed directly on an
  `if` expression is rejected. Wrap the `if` in a block, or use `cfg!()`.
- **Ctrl+C interrupts.** `extra_rx_ctrl_c_byte_interrupts_pump` (`:831`) and
  `extra_rx_kitty_csi_u_ctrl_c_interrupts_pump` (`:912`) cover the
  interrupt path through `extra_rx`.
- **Git Bash / mintty without winpty** gives pipe stdio. clud then falls back
  to subprocess mode (#1357; see windows-quirks.md).
