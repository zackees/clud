# dnd/

Drag-and-drop handling for terminal-embedded `clud` sessions. Provides two complementary paths: (1) a cross-platform string normalizer that canonicalizes the path-shaped byte sequences each terminal injects on a drop (cmd.exe quoted paths, mintty `/c/...` MSYS paths, PowerShell `& 'C:\...'`, macOS backslash-escaped spaces, GNOME `file://` URIs), and (2) a Windows-only OLE `IDropTarget` adapter that intercepts drops at the COM layer (fixing issue #65 where conhost rejects the drop) and forwards parsed paths to a per-launch-mode injector (subprocess via `WriteConsoleInputW`, PTY via the master writer).

The Windows COM lifecycle (`OleInitialize` worker thread, `RegisterDragDrop` displacement strategy for issue #79, `IDropTarget` vtable, RAII teardown) is covered in [docs/architecture/windows-quirks.md](../../../../docs/architecture/windows-quirks.md). The PTY-side injection path (how bytes reach the master) is in [docs/architecture/session-lifecycle.md](../../../../docs/architecture/session-lifecycle.md).

## Files

- `mod.rs` — public `normalize_dropped_path` / `looks_like_dropped_path` string transforms plus `pub mod` re-exports of the submodules.
- `dropfiles.rs` — pure `&[u8]` parser for the Win32 `CF_HDROP` / `DROPFILES` wire format (wide + narrow encodings); panic-free on malformed input.
- `console_drop_target.rs` — Windows-only `IDropTarget` COM object, `OleInitialize`/`RegisterDragDrop` worker thread with delay-then-refresh strategy (issue #79, displaces Claude Code's own registration), RAII guard, and the platform-agnostic dispatch glue and `DROPEFFECT` decision.
- `console_drop_target_tests.rs` — tests on every host: dispatch, `drag_effect`, `RefreshConfig`, and the registration loop against `MockRegistrar`.
- `console_drop_target_com_tests.rs` — Windows-only tests of the real COM layer (#1362); see [Testing](#testing).
- `drop_host.rs` — pure host decision (Windows or test builds only): whether the `IDropTarget` also covers the Windows Terminal window, or stays on `GetConsoleWindow()` for VS Code, WezTerm and conhost (#1358). Inputs are an injected env reader and process snapshot; tests in `drop_host_tests.rs` run on every host.
- `injectors_win_tests.rs` — Windows-only tests of the subprocess-mode `WriteConsoleInputW` path against a real `CONIN$` (#1370); see [Testing](#testing).
- `injectors.rs` — `DropInjector` factories for the two launch modes plus `build_input_records` (synthesizes Win32 `INPUT_RECORD` bytes for `WriteConsoleInputW`) and `join_paths_for_injection` (newline-join + trailing space contract).

## Key items

- `normalize_dropped_path(input: &str) -> String` — `mod.rs:49`
- `looks_like_dropped_path(input: &str) -> bool` — `mod.rs:77`
- `parse_dropfiles_buffer(buf: &[u8]) -> Vec<String>` — `dropfiles.rs:39`
- `DROPFILES_HEADER_SIZE` / `DROPFILES_PFILES_OFFSET` / `DROPFILES_FWIDE_OFFSET` — `dropfiles.rs:28-32`
- `pub type DropInjector` — `console_drop_target.rs:78`
- `enum RegisterError` — `console_drop_target.rs:82`
- `struct RefreshConfig` with `default_displacement()` (2s/3s) and `immediate_no_refresh()` — `console_drop_target.rs:145`
- `struct ConsoleDropTargetGuard` (RAII; signals worker, revokes, `OleUninitialize`) — `console_drop_target.rs:335`
- `register_console_drop_target(injector, config)` — `console_drop_target.rs:386` (Windows) / `:394` (non-Windows stub)
- `resolve_drop_host(env, current_pid, processes) -> DropHost` — `drop_host.rs:59`
- `dispatch_dropfiles_to_injector(buf, injector) -> bool` (whether the injector fired) — `console_drop_target.rs:413`
- `drag_effect(source_allowed, carries_files)` — the `DROPEFFECT` every `IDropTarget` method reports: `COPY` only for a `CF_HDROP` payload from a source that allows a copy, else `NONE`; never `MOVE` — `console_drop_target.rs:448`
- `win::new_drop_target(injector) -> IDropTarget` (Windows) — `console_drop_target.rs:500`
- `win::copy_cf_hdrop_bytes(data)` (Windows; `GetData` → `GlobalLock` → copy `GlobalSize` bytes → `GlobalUnlock` → `ReleaseStgMedium`) — `console_drop_target.rs:651`
- `build_input_records(s: &str) -> Vec<u8>` — `injectors.rs:71`
- `join_paths_for_injection(paths: &[String]) -> String` — `injectors.rs:126`
- `pty_master_injector(master) -> DropInjector` — `injectors.rs:138`
- `subprocess_console_injector() -> DropInjector` (Windows only) — `injectors.rs:189`
- `write_to_console_input(records_bytes: &[u8]) -> io::Result<()>` (Windows only) — `injectors.rs:228`
- `input_record_count(len)` / `check_all_records_written(expected, written)` — pure length and short-write checks, tested on every host — `injectors.rs:158`, `:175`
- `console_input_injector(resolve)` / `decode_input_records(bytes)` / `write_records_to_handle(handle, bytes)` (Windows only; records are decoded field by field, never pointer-cast, since a `&[u8]` need not be 4-byte aligned) — `injectors.rs:197`, `:244`, `:276`
- `INPUT_RECORD_SIZE = 20` — `injectors.rs:58`

## Testing

- **Every host:** the `DROPFILES` parser (`dropfiles.rs`), `dispatch_dropfiles_to_injector`, `drag_effect`, the host decision (`drop_host_tests.rs`) and the registration loop against `MockRegistrar`.
- **Windows unit lane:** `console_drop_target_com_tests.rs` drives the real `IDropTarget` vtable from `win::new_drop_target` (`QueryInterface`/`AddRef`/`Release`, `DragEnter`/`DragOver`/`DragLeave`/`Drop` and their effects, a null effect pointer, a panicking injector) with `FakeDataObject`, an in-process `IDataObject` serving an `STGMEDIUM` the test builds. It checks the `FORMATETC` requested, that the copy spans `GlobalSize`, that the lock count returns to zero, and that `ReleaseStgMedium` releases the medium exactly once. It needs no console window, so it runs on headless CI runners.
- **Windows unit lane, console input:** `injectors_win_tests.rs` writes through `write_records_to_handle`, `write_to_console_input` and `subprocess_console_injector` into this process's `CONIN$` (allocating a console with `AllocConsole` when the runner started the binary without one; it fails rather than skips if neither works) and reads the records back with `ReadConsoleInputW`. `STD_INPUT_HANDLE` is pointed at `CONIN$` with `SetStdHandle` for the stdin-based entry points. `a_real_drop_types_the_paths_into_the_console_input_buffer` in `console_drop_target_com_tests.rs` drives a real `Drop` through the vtable into that buffer end to end. All of them hold one lock, since the buffer is process-wide.
- **Manual only:** the real `RegisterDragDrop` / `RevokeDragDrop` round trip through `OleRegistrar` and a drag from Explorer onto a live console or Windows Terminal window. Both need an interactive desktop session; CI runners have no console window, so `register_console_drop_target` returns `ConsoleWindowUnavailable` there.

## Used by

- `session.rs` — calls `looks_like_dropped_path` + `normalize_dropped_path` on PTY paste-buffer input to wrap dropped paths in bracketed-paste markers (`session.rs:12`, `:839`).
- `startup.rs` — wires `register_console_drop_target` for both launch modes via `try_register_console_drop_target_subprocess` (uses `subprocess_console_injector`) and `try_register_console_drop_target_pty` (uses `pty_master_injector`) at `startup.rs:32`, `:56`.
- `main.rs` — invokes `startup::try_register_console_drop_target_subprocess()` for subprocess launches (`main.rs:267`).
- `runner.rs` — invokes `startup::try_register_console_drop_target_pty()` for PTY launches (`runner.rs:438`).
- `lib.rs` — declares `pub mod dnd` (`lib.rs:15`).
