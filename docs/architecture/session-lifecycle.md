# Session Lifecycle

From the moment `runner::run_plan_pty` asks `NativePtyProcess::new` for a child
PTY, clud owns the bidirectional byte stream between the user's terminal and
the backend agent (`claude` or `codex`). `console_setup` flips the Windows
console into VT-input mode under an RAII guard, `console_title` stamps the
title and arms a keeper, then `session::run_raw_pty_pump` runs the cooperative
input/output/resize loop. Inside that loop, `dnd::injectors::pty_master_injector`
feeds drag-drop bytes via a side channel, `voice::VoiceMode` consumes F3 events
from `session::F3Observer` and writes transcripts back to the PTY master, and
`console_title::OscTitleStripper` removes the backend's OSC 0/2 title noise
before it can reach the user's terminal. The session ends when the child
exits, when Ctrl+C is observed, or when the iteration loop in `runner.rs`
moves on; the RAII guards restore console mode on drop.

## Component map

| File | Role in the session |
|---|---|
| `crates/clud-bin/src/runner.rs` | `run_plan_pty` allocates the PTY, holds `_console_guard` + `_raw_guard` + dnd registration for the iteration, calls `run_raw_pty_pump_with_extra_rx_verbose`. |
| `crates/clud-bin/src/session.rs` | The pump loop, `F3Observer`, `BracketedPasteNormalizer`, `resize_pty`, `spawn_os_resize_watcher`, `interrupt_pty_process`, `RawTerminalGuard`. |
| `crates/clud-bin/src/console_setup.rs` | `ConsoleVtGuard` RAII for `ENABLE_VIRTUAL_TERMINAL_INPUT`. |
| `crates/clud-bin/src/console_title.rs` | One-shot title stamp, daemon keeper thread, `OscTitleStripper` stream filter. |
| `crates/clud-bin/src/console_input.rs` | Issue #141 Shift+Enter translator (`KEY_EVENT_RECORD` → bytes). |
| `crates/clud-bin/src/capture.rs` | `TerminalCapture` — `vt100` parser plus the sticky-mode `vte` sniffer. Used by the daemon worker for attach repaint. |
| `crates/clud-bin/src/dnd/injectors.rs` | `pty_master_injector` adapter that the OLE callback writes through. |
| `crates/clud-bin/src/voice/mode.rs` | `VoiceMode::on_f3_press` / `on_f3_release` / `on_tick`, the only `InteractiveHooks` implementor in production. |

## Startup

`main.rs` stamps `clud <cwd-name>` and spawns the keeper before any backend
process exists:

- `console_title::set_for_current_cwd` (`console_title.rs:48`) writes the
  desired title to a shared `Mutex<String>` and calls `SetConsoleTitleW`.
- `console_title::keep_setting_in_background` (`console_title.rs:70`) uses
  `OnceLock` to spawn the daemon thread at most once per process; the thread
  re-stamps the title every 750 ms if the live console title has drifted.

When the PTY branch is selected, `runner::run_plan_pty` (`runner.rs:415`)
arms the per-session guards in this order before allocating the PTY:

1. `_console_guard = enable_console_vt_input()` (`runner.rs:429`,
   `console_setup.rs:26`) — sets `ENABLE_VIRTUAL_TERMINAL_INPUT` (0x0200) on
   the stdin console handle and captures the original mode. Without this bit,
   `ReadConsoleW` delivers ANSI sequences that the backend's TUI cannot parse,
   and Backspace arrives as 0x08 instead of the xterm-style 0x7f that
   Ink-based UIs expect. No-op on POSIX.
2. The optional dnd registration (`runner.rs:436-446`) — guarded behind
   `--no-dnd` / `--dry-run`. Holds an `Option<ConsoleDropTargetGuard>` plus a
   `Receiver<Vec<u8>>` for the duration of the launch.
3. `NativePtyProcess::new` (`runner.rs:482`) at the resolved
   `get_terminal_size()` (`runner.rs:42`), then `process.set_echo(false)`
   (`runner.rs:509`) so the library's built-in stdout writer is silent and
   the pump owns forwarding.
4. `_raw_guard = session::enter_raw_mode_if_tty()` (`runner.rs:526`,
   `session.rs:273`) — `crossterm::terminal::enable_raw_mode` plus
   `PushKeyboardEnhancementFlags(REPORT_EVENT_TYPES |
   DISAMBIGUATE_ESCAPE_CODES)` so the kitty keyboard protocol carries F3
   release events through. Dropped at `runner.rs:541`.

## The pump loop (`run_raw_pty_pump`)

The pump entry chain is `run_raw_pty_pump` (`session.rs:349`) →
`run_raw_pty_pump_with_extra_rx` (`session.rs:370`) →
`run_raw_pty_pump_with_extra_rx_verbose` (`session.rs:392`), which constructs
the resize channel and spawns the watcher before calling
`run_raw_pty_pump_full_verbose` (`session.rs:513`). One iteration of the inner
loop does, in order:

**Output side** (`session.rs:585-597`):
`process.read_chunk_impl(Some(0.01))` drains one chunk of child output. The
chunk runs through `OscTitleStripper::process` (`console_title.rs:204`); the
filtered bytes are written to `io::stdout().lock()`. The library still keeps
the un-filtered chunk in its internal `chunks` queue, which is what
`TerminalCapture` consumes on the daemon path — see "Capture for attach".

**Resize channel** (`session.rs:601-605`): drained before stdin so a
late-arriving resize doesn't wait on a typing chunk. `resize_pty`
(`session.rs:20`) unwraps `process.handles.lock()` and calls
`master.resize(PtySize { rows, cols, .. })` directly on Windows because
`NativePtyProcess::resize_impl` is a deliberate no-op there; POSIX delegates
to the library. Issue #31 T2.

**Drag-drop side channel** (`session.rs:616-625`): one chunk per iteration
out of `extra_rx`. The bytes are already newline-joined and trailing-space
terminated by `dnd::injectors::join_paths_for_injection` and are forwarded
straight to `process.write_impl(&chunk, false)` — bypassing the
bracketed-paste normalizer because the OLE callback has already canonicalized
them.

**Input side** (`session.rs:627-665`): one chunk from `stdin_rx`. The chunk
runs through `BracketedPasteNormalizer::process` (`session.rs:734`), which
detects `\x1b[200~ … \x1b[201~` envelopes and, when the inner content matches
`dnd::looks_like_dropped_path`, rewrites it through `normalize_dropped_path`
before forwarding. Non-paste bytes pass through with O(1) cost. The
normalized chunk is then written to the PTY master with `write_impl(..,
false)`.

**Voice F3 hook insertion** (`session.rs:637-652`): when
`hooks.intercept_f3()` returns true, the same chunk (pre-normalization,
because we want detection symmetry with raw byte forwarding) is fed to
`F3Observer::observe`. Each reported press fires `hooks.on_f3_press(process)`;
each release fires `hooks.on_f3_release(process)`. Repeats are silently
dropped — they signal autorepeat, not a fresh press.

**Ctrl+C cooperation** (`session.rs:654-664`): `stdin_chunk_requests_interrupt`
flags a 0x03 byte in the chunk. If set, or if the external `interrupted`
atomic flips, the pump calls `interrupt_pty_process` (`session.rs:856`) and
returns. On Windows the escalation closes the PTY (ConPTY translates that
into `CTRL_CLOSE_EVENT` to the child); on POSIX it sends SIGINT to the
child's pgroup and waits up to 2 s.

**Tick + child-exit check** (`session.rs:667-685`): `hooks.on_tick` always
runs, even on idle iterations, so VAD auto-stop and voice transcript draining
make progress. `poll_pty_process` checks for child exit and returns the code.

## Capture for attach

`TerminalCapture` (`capture.rs:22`) is the daemon worker's emulator. The
worker feeds every PTY output chunk into `TerminalCapture::feed`
(`capture.rs:101`), which drives a `vt100::Parser` for the cell grid plus a
parallel `vte::Parser` (`StickySniffer`) that tracks two modes vt100 0.16
doesn't round-trip — `DECSTBM` scroll region and `DECAWM` autowrap-off.

When a `clud attach` client connects mid-session, the daemon calls
`TerminalCapture::snapshot_bytes` (`capture.rs:132`) to synthesize a repaint:

1. `\x1bc` (RIS) to reset SGR, modes, scroll region, cursor style.
2. `\x1b[?1049h` if the app is on the alternate screen, so the cells land on
   the alt buffer.
3. The sticky `DECSTBM` and `DECAWM` re-asserts, since RIS clears them.
4. `screen.state_formatted()` — cells, SGR, cursor position, bracketed-paste,
   application cursor, application keypad, mouse protocol mode.

A fresh terminal replaying that byte stream ends up at the same final frame
the source TUI is currently rendering. Known limitations (window title, saved
cursor register, DEC graphics charset, cursor shape) are documented inline in
`capture.rs`; see issue #36.

## Input injection

Three sources can put bytes onto the PTY master during a session. Two run on
the pump thread (synchronous, single-writer), one runs on the GUI thread
that owns the console window:

1. **Keyboard** — `stdin_source` (production = `io::stdin`) is read by the
   detached reader thread at `session.rs:544-562`. On Windows interactive
   stdin, `normalize_interactive_console_stdin_chunk` rewrites 0x08 → 0x7f so
   Backspace aligns with xterm. The Shift+Enter translation in
   `console_input::translate` (`console_input.rs:71`) maps `VK_RETURN` +
   `SHIFT_PRESSED` → `\n` and plain Enter → `\r`. The translator is a pure
   function over `&[InputEvent]`; the thread that calls `ReadConsoleInputW`
   and feeds it lands in a follow-up patch — see issue #141.
2. **Drag-and-drop** — `pty_master_injector`
   (`crates/clud-bin/src/dnd/injectors.rs:138`) wraps the PTY master in
   `Arc<Mutex<Box<dyn Write + Send>>>` so the OLE `IDropTarget::Drop`
   callback can call `guard.write_all(payload.as_bytes())` directly. The
   payload is `join_paths_for_injection(paths)` (newline-joined, trailing
   space). In the pump loop, `extra_rx` delivers an equivalent
   pre-normalized chunk via the side channel
   (`runner.rs:438`, `session.rs:616`). The COM/IDropTarget side belongs to
   `windows-quirks.md`; this doc covers only the "bytes get written to the
   PTY master" path.
3. **Voice** — `voice::VoiceMode` (`crates/clud-bin/src/voice/mode.rs:16`)
   implements `InteractiveHooks` (`voice/mode.rs:194`). `on_f3_press`
   starts recording, `on_f3_release` plays the stop cue and enqueues
   audio on the `VoiceWorker` thread, and `on_tick` drains
   `WorkerEvent::Transcript` and writes the transcript via
   `process.write_impl(trimmed.as_bytes(), false)` (`voice/mode.rs:175`).
   `false` means "not a paste", so the text appears at the cursor without
   bracketed-paste markers and the backend prompt does not auto-submit.

## Title management

`set_for_current_cwd` (`console_title.rs:48`) runs once at process start and
records the desired title in a static `Mutex<String>`. The keeper thread
spawned by `spawn_keeper_thread` (`console_title.rs:76`) polls
`GetConsoleTitleW` every 750 ms and re-stamps when the live title has
drifted. This is the only defense in subprocess mode, where the child
inherits stdio handles directly and clud cannot intercept OSC bytes.

In PTY mode the pump runs every output chunk through `OscTitleStripper`
before stdout. The stripper is a stream-resumable state machine
(`console_title.rs:176`) with seven states (`Normal`, `AfterEsc`,
`InOscNumber`, `SwallowOscBody`, `SwallowAfterEsc`, `PassthroughOscBody`,
`PassthroughAfterEsc`). It drops OSC 0 (icon + title) and OSC 2 (title only)
sequences terminated by `BEL` (0x07) or `ST` (`ESC \\`), and passes
everything else through verbatim — including OSC 8 hyperlinks, OSC 10/11
color queries, OSC 52 clipboard, and OSC 133 prompt marks. With the stripper
in place the keeper rarely fires; it is the safety net for sequences split
across reads or for terminals that bypass our stdout.

## Shutdown

The pump exits in one of three ways:

- **Child exit**: `poll_pty_process` (`session.rs:671`) returns
  `Ok(Some(code))`. The pump returns the code; `run_plan_pty` runs it
  through `normalize_exit_code` (`runner.rs:79`).
- **Ctrl+C cooperation**: either a 0x03 byte on stdin or the external
  `interrupted` atomic flipping. The pump calls `interrupt_pty_process`
  (`session.rs:856`). On Windows the helper closes the PTY (ConPTY's
  `CTRL_CLOSE_EVENT` path) so the child does not receive a second 0x03 that
  Ink-based TUIs interpret as "Ctrl+C twice = exit". On POSIX it sends
  SIGINT to the child's pgroup and waits up to 2 s before falling back to
  `close_impl`. Returns 130.
- **PTY read error**: `read_chunk_impl` returning `Err` is treated as a
  child-gone signal; the pump calls `reap_pty_exit` (`session.rs:848`) which
  invokes `wait_impl(Some(1.0))` and returns 1 on timeout.

On every return, `_raw_guard` drops first (`runner.rs:541`), restoring
crossterm raw mode and popping keyboard enhancement flags. The
`_dnd_pty_guard` and `_console_guard` drop when the `run_plan_pty` frame
unwinds: `ConsoleDropTargetGuard::Drop` signals the worker thread, revokes
the `IDropTarget`, and calls `OleUninitialize`; `ConsoleVtGuard::Drop`
(`console_setup.rs:13`) restores the captured original console mode bits.
The resize-watcher thread observes the closed `resize_tx` and exits.

## Key types

| Symbol | Location |
|---|---|
| `run_raw_pty_pump` | `crates/clud-bin/src/session.rs:349` |
| `run_raw_pty_pump_with_extra_rx_verbose` | `crates/clud-bin/src/session.rs:392` |
| `run_raw_pty_pump_full_verbose` (inner loop) | `crates/clud-bin/src/session.rs:513` |
| `F3Observer` struct | `crates/clud-bin/src/session.rs:92` |
| `F3Observer::observe` | `crates/clud-bin/src/session.rs:125` |
| `InteractiveHooks` trait | `crates/clud-bin/src/session.rs:243` |
| `BracketedPasteNormalizer` | `crates/clud-bin/src/session.rs:734` |
| `resize_pty` | `crates/clud-bin/src/session.rs:20` |
| `spawn_os_resize_watcher` | `crates/clud-bin/src/session.rs:431` |
| `interrupt_pty_process` | `crates/clud-bin/src/session.rs:856` |
| `RawTerminalGuard` | `crates/clud-bin/src/session.rs:265` |
| `TerminalCapture` | `crates/clud-bin/src/capture.rs:22` |
| `TerminalCapture::snapshot_bytes` | `crates/clud-bin/src/capture.rs:132` |
| `ConsoleVtGuard` | `crates/clud-bin/src/console_setup.rs:8` |
| `enable_console_vt_input` | `crates/clud-bin/src/console_setup.rs:26` |
| `console_input::translate` | `crates/clud-bin/src/console_input.rs:71` |
| `console_title::set_for_current_cwd` | `crates/clud-bin/src/console_title.rs:48` |
| `console_title::keep_setting_in_background` | `crates/clud-bin/src/console_title.rs:70` |
| Title keeper thread entry (Windows) | `crates/clud-bin/src/console_title.rs:76` |
| `OscTitleStripper` | `crates/clud-bin/src/console_title.rs:176` |
| `VoiceMode` (`InteractiveHooks` impl) | `crates/clud-bin/src/voice/mode.rs:194` |
| `pty_master_injector` | `crates/clud-bin/src/dnd/injectors.rs:138` |
| `run_plan_pty` (call site) | `crates/clud-bin/src/runner.rs:415` |

## Failure modes

- **PTY allocation fails.** `NativePtyProcess::new` returns `Err`;
  `run_plan_pty` logs `[clud] failed to create pty` and returns 1 without
  spinning the pump (`runner.rs:491-501`). Nested Windows shells where
  ConPTY silently no-ops are the usual cause; the unit test in `session.rs`
  detects this and skips.
- **vt100 parse error.** The vt100 parser is a streaming VTE and recovers
  from any byte sequence; it cannot panic on input. A malformed CSI just
  resets parser state and the next valid sequence renders correctly. See
  `capture.rs:malformed_csi_is_recovered_from`.
- **Child crashes.** `read_chunk_impl` returns `Err` and the pump calls
  `reap_pty_exit` (`session.rs:848`), which waits 1 s before returning 1.
  `process_tree::kill_tree` runs from the outer Ctrl+C path, not from a
  child-exit path.
- **Hot resize during a write.** Resize events are drained before stdin in
  every loop iteration (`session.rs:601`), so the next write goes to a
  correctly-sized PTY. A resize that fires mid-`write_impl` is harmless —
  ConPTY serializes the master writer, and the new size only affects
  subsequent writes.
- **OSC sequence split mid-buffer.** Both `OscTitleStripper` and
  `BracketedPasteNormalizer` are stream-resumable; the unit tests cover
  byte-by-byte fragmentation. A title `ESC ] 0 ; … BEL` split across two
  `read_chunk_impl` calls still gets swallowed exactly once.
- **F3 sequence split mid-buffer.** `F3Observer` keeps state across
  `observe` calls; even one-byte-at-a-time fragmentation of `\x1bOR` or
  `\x1b[13;1:3~` fires exactly one event. CSI parameter overrun is bounded
  by `MAX_CSI_LEN = 64` (`session.rs:113`).

## See also

- [`../../crates/clud-bin/src/dnd/README.md`](../../crates/clud-bin/src/dnd/README.md)
  — `IDropTarget` adapter, `DropInjector` factories, COM lifecycle.
- [`../../crates/clud-bin/src/voice/README.md`](../../crates/clud-bin/src/voice/README.md)
  — F3 hold-to-record, Whisper worker thread, model auto-download.
- [`daemon-ipc.md`](daemon-ipc.md) — the attach flow that consumes
  `TerminalCapture::snapshot_bytes`.
- [`windows-quirks.md`](windows-quirks.md) — key translation rationale,
  console mode bits, ConPTY resize no-op, `CTRL_CLOSE_EVENT` semantics.
- [`../DESIGN_DECISIONS.md`](../DESIGN_DECISIONS.md) — ADR-style records for
  the raw-pump refactor (issue #46), the OSC stripper / keeper split, and
  the voice F3 observer/hook design.
