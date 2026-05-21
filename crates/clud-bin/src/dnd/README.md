# dnd/

Drag-and-drop handling for terminal-embedded `clud` sessions. Provides two complementary paths: (1) a cross-platform string normalizer that canonicalizes the path-shaped byte sequences each terminal injects on a drop (cmd.exe quoted paths, mintty `/c/...` MSYS paths, PowerShell `& 'C:\...'`, macOS backslash-escaped spaces, GNOME `file://` URIs), and (2) a Windows-only OLE `IDropTarget` adapter that intercepts drops at the COM layer (fixing issue #65 where conhost rejects the drop) and forwards parsed paths to a per-launch-mode injector (subprocess via `WriteConsoleInputW`, PTY via the master writer).

## Files

- `mod.rs` — public `normalize_dropped_path` / `looks_like_dropped_path` string transforms plus `pub mod` re-exports of the submodules.
- `dropfiles.rs` — pure `&[u8]` parser for the Win32 `CF_HDROP` / `DROPFILES` wire format (wide + narrow encodings); panic-free on malformed input.
- `console_drop_target.rs` — Windows-only `IDropTarget` COM object, `OleInitialize`/`RegisterDragDrop` worker thread with delay-then-refresh strategy (issue #79, displaces Claude Code's own registration), RAII guard, and the platform-agnostic dispatch glue.
- `injectors.rs` — `DropInjector` factories for the two launch modes plus `build_input_records` (synthesizes Win32 `INPUT_RECORD` bytes for `WriteConsoleInputW`) and `join_paths_for_injection` (newline-join + trailing space contract).

## Key items

- `normalize_dropped_path(input: &str) -> String` — `mod.rs:49`
- `looks_like_dropped_path(input: &str) -> bool` — `mod.rs:77`
- `parse_dropfiles_buffer(buf: &[u8]) -> Vec<String>` — `dropfiles.rs:39`
- `DROPFILES_HEADER_SIZE` / `DROPFILES_PFILES_OFFSET` / `DROPFILES_FWIDE_OFFSET` — `dropfiles.rs:28-32`
- `pub type DropInjector` — `console_drop_target.rs:76`
- `enum RegisterError` — `console_drop_target.rs:80`
- `struct RefreshConfig` with `default_displacement()` (2s/3s) and `immediate_no_refresh()` — `console_drop_target.rs:143`
- `struct ConsoleDropTargetGuard` (RAII; signals worker, revokes, `OleUninitialize`) — `console_drop_target.rs:333`
- `register_console_drop_target(injector, config)` — `console_drop_target.rs:384` (Windows) / `:392` (non-Windows stub)
- `dispatch_dropfiles_to_injector(buf, injector)` — `console_drop_target.rs:407`
- `build_input_records(s: &str) -> Vec<u8>` — `injectors.rs:71`
- `join_paths_for_injection(paths: &[String]) -> String` — `injectors.rs:126`
- `pty_master_injector(master) -> DropInjector` — `injectors.rs:138`
- `subprocess_console_injector() -> DropInjector` (Windows only) — `injectors.rs:157`
- `write_to_console_input(records_bytes: &[u8]) -> io::Result<()>` (Windows only) — `injectors.rs:176`
- `INPUT_RECORD_SIZE = 20` — `injectors.rs:58`

## Used by

- `session.rs` — calls `looks_like_dropped_path` + `normalize_dropped_path` on PTY paste-buffer input to wrap dropped paths in bracketed-paste markers (`session.rs:12`, `:839`).
- `startup.rs` — wires `register_console_drop_target` for both launch modes via `try_register_console_drop_target_subprocess` (uses `subprocess_console_injector`) and `try_register_console_drop_target_pty` (uses `pty_master_injector`) at `startup.rs:32`, `:56`.
- `main.rs` — invokes `startup::try_register_console_drop_target_subprocess()` for subprocess launches (`main.rs:267`).
- `runner.rs` — invokes `startup::try_register_console_drop_target_pty()` for PTY launches (`runner.rs:438`).
- `lib.rs` — declares `pub mod dnd` (`lib.rs:15`).
