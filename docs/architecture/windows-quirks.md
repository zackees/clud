# Windows Quirks

This doc is the inventory of every place `clud` has Windows-specific code,
with the symptom each piece solves and the `file:line` where it lives. There
are ten such carve-outs today: a self-rename trampoline so `pip install`
can overwrite a running `clud.exe`, the BatBadBat `.cmd`/`.bat` rewrite
mandated by Rust 1.77+, an RAII guard for `ENABLE_VIRTUAL_TERMINAL_INPUT`, a
`ReadConsoleInputW` translator that disambiguates Shift+Enter from plain
Enter, a console-title keeper that re-stamps `clud <cwd>` when child TUIs
overwrite it, an OLE `IDropTarget` adapter so dragging a file onto the
console window actually drops paths into the prompt, `CREATE_NO_WINDOW` for
daemon-helper subprocesses that would otherwise flash a conhost window, a
`whisper-rs` carve-out on `aarch64-pc-windows-msvc` where the sys-crate
doesn't build, a Ctrl+C descendant-tree teardown that reaps orphaned backend
grandchildren without tripping cmd.exe's batch-job prompt, and a Codex
`PreToolUse` hook diagnostic for batch wrappers that do not propagate
`$LASTEXITCODE`. All ten degrade to no-ops (or different mechanisms entirely)
on POSIX.

## Why so many?

Each of these is individually small. Cumulatively they exist because Windows
differs from POSIX in six distinct ways `clud` cares about: ConPTY
semantics are not VT100 (so we have to opt into virtual-terminal input and
re-translate console input records); COM (`IDropTarget`, `OleInitialize`)
is the only supported integration point for drag-and-drop into a console
window; running executables are file-locked, so the standard
`pip install --force-reinstall` overwrite path silently fails; cmd.exe's
command-line parser is idiosyncratic enough that Rust's stdlib
(post-CVE-2024-24576) refuses to launch a `.cmd` directly; PowerShell and
batch-wrapper exit-code propagation can mask a failed native hook; and the
console process group sends `CTRL_C_EVENT` to every attached process, including
grandchildren we don't directly control. None of these are bugs in `clud`;
they are platform contracts that we absorb in one module each so the rest of
the codebase stays portable.

## Inventory

### (a) Trampoline: exe self-rename for `pip install` overwrite

- **Symptom**: `pip install --force-reinstall clud` fails with a permission
  error when any `clud.exe` is already running, because Windows file-locks
  every running executable. The error is surfaced by pip with a generic
  "could not install" message that doesn't make the root cause obvious.

- **Solution**: At the top of `main`, rename `Scripts/clud.exe` to
  `Scripts/clud.exe.old.<rand>` and copy the renamed file back to
  `clud.exe`. The running process continues executing from the
  `.old.<rand>` file (which is now the locked one), while `Scripts/clud.exe`
  becomes a fresh, unlocked copy that `pip` can overwrite. A background
  thread GCs stale `.old.*` files on the next launch. The detached-spawn
  helper additionally strips `HANDLE_FLAG_INHERIT` from the parent's three
  stdio handles around `CreateProcess` so the child cannot keep a pipe
  writer alive past EOF — that was the root cause of the 45-minute Windows
  GHA cancellation investigated in issue #37.

- **File**: `crates/clud-bin/src/trampoline.rs:141` (`unlock_exe`); detached
  spawn at `:39` (`spawn_detached_self`); the RAII handle-flag guard at
  `:75` (`windows_stdio::NonInheritableStdioGuard`).

- **POSIX behavior**: No-op. Unix lets you `unlink` a running binary; the
  rename dance is unnecessary, so `unlock_exe()` returns immediately on the
  `cfg!(target_os = "windows")` check at `:142`. The detached-spawn helper
  also has a `#[cfg(unix)]` branch at `:55` that uses `setsid()` instead
  of the Windows `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` flags.

### (b) BatBadBat: `.cmd`/`.bat` rewrite (CVE-2024-24576)

- **Symptom**: `clud --codex` fails with `failed to spawn process: batch
  file arguments are invalid`. Since Rust 1.77 (the CVE-2024-24576 fix),
  `std::process::Command` refuses any batch invocation whose arguments
  don't round-trip losslessly through cmd.exe's parser. npm installs Codex
  as a `.cmd` shim at `%APPDATA%\npm\codex.cmd`, which triggers exactly
  this refusal (issue #59).

- **Solution**: Rewrite the launch as `cmd.exe /D /S /C "<bat-path>" <args>`
  so Rust is spawning cmd.exe (a real `.exe`) and the batch invocation
  lives inside a shell command line where `clud` controls the quoting.
  `running-process-core::CommandSpec::Shell` already provides the outer
  wrapper via `raw_arg` (it builds `cmd /D /S /C "<command>"` verbatim);
  `subprocess.rs` is the *single decision point* that picks `Shell` vs
  `Argv` based on the `argv[0]` extension. Inside the quoted region each
  token is independently wrapped in `"..."` with `%` → `%%` and `"` → `""`
  escapes; everything else (`&`, `|`, `<`, `>`, `^`, `;`) stays literal
  thanks to the outer quotes. The `/D` flag suppresses any user-installed
  `AutoRun` registry key; `/S` makes cmd's quote handling predictable
  (outermost `"..."` stripped verbatim); `/C` runs and exits.
  `subprocess::argv_is_batch_wrapped` exposes the same `.cmd` / `.bat`
  decision to the Ctrl+C teardown path so clud can treat the intermediate
  `cmd.exe` differently from a native backend executable.

- **File**: `crates/clud-bin/src/subprocess.rs:34`
  (`command_spec_for_subprocess`); the case-insensitive `.cmd`/`.bat` check
  at `:47` (`is_windows_batch_wrapper`); per-arg quoting at `:106`
  (`quote_for_cmd`); `argv_is_batch_wrapped` for Ctrl+C teardown gating.

- **POSIX behavior**: No-op. `is_windows_batch_wrapper` is gated
  `#[cfg(windows)]`; on POSIX every argv stays as `CommandSpec::Argv` and a
  file literally named `codex.cmd` is just treated as an executable (test
  at `subprocess.rs:294`).

### (c) `ENABLE_VIRTUAL_TERMINAL_INPUT` RAII

- **Symptom**: Inside `clud --codex`, Backspace doesn't delete anything in
  the Ink TUI. The cause: without `ENABLE_VIRTUAL_TERMINAL_INPUT` (0x0200)
  on the console input handle, `ReadConsoleW` delivers Backspace as `0x08`,
  but xterm-style TUIs (Ink, Codex) expect `0x7F`. The same bit is also
  required for bracketed-paste and other ANSI input sequences to pass
  through unmangled.

- **Solution**: An RAII guard (`ConsoleVtGuard`) that ORs
  `ENABLE_VIRTUAL_TERMINAL_INPUT` into the console-input mode for the
  lifetime of a PTY session and `SetConsoleMode`-restores the saved
  original on drop. The guard returns early without touching the mode if
  stdin is not a real TTY (piped `cargo test`, CI without a console) —
  it remembers `original_mode: None` so the drop impl skips the restore.

- **File**: `crates/clud-bin/src/console_setup.rs:8` (`ConsoleVtGuard`);
  construction at `:26` (`enable_console_vt_input`); the actual
  `Get/SetConsoleMode` calls at `:51` (`set_console_vt_input`) and `:82`
  (`restore_console_mode`).

- **POSIX behavior**: No-op. The `ConsoleVtGuard` struct has no
  `original_mode` field off Windows (see the `#[cfg(windows)]` field at
  `:9`); the `Drop` impl is empty on POSIX; the `enable_console_vt_input`
  constructor at `:44` returns the empty-struct form. POSIX terminals are
  already in canonical VT mode and need no opt-in.

### (d) Shift+Enter via `ReadConsoleInputW` (issue #141)

- **Symptom**: Pressing Shift+Enter in `clud` should insert a literal
  newline into the backend's prompt (so the user can type a multi-line
  message), but conhost strips modifier-key state before producing the
  `\r` byte on stdin. A byte-stream reader can't distinguish Enter from
  Shift+Enter.

- **Solution**: Read input via `ReadConsoleInputW` (which exposes
  `KEY_EVENT_RECORD::dwControlKeyState`) and translate the records to
  PTY-stdin bytes:
  - `VK_RETURN` (0x0D) key-down with `SHIFT_PRESSED` (0x0010) set → `\n`
    (0x0A) — the "insert newline in the prompt" byte.
  - `VK_RETURN` key-down without `SHIFT_PRESSED` → `\r` (0x0D) — the usual
    "submit" byte. Ctrl+Enter and Alt+Enter intentionally fall through to
    plain `\r` so we don't silently change behavior of any existing
    backend binding.
  - Any other key-down with a non-zero `unicode_char` emits the UTF-8
    encoding of that UTF-16 code unit.
  - Key-up events and non-key records (mouse, focus, buffer-size, menu,
    window) are dropped at the `InputEvent::NonKey` variant.

  The translator itself is a pure function
  (`translate(&[InputEvent]) -> Vec<u8>`), which is what makes the unit
  tests runnable on every CI host.

- **File**: `crates/clud-bin/src/console_input.rs:71` (`translate`);
  `VK_RETURN` constant at `:36`; `SHIFT_PRESSED` constant at `:40`. The
  whole module is gated `#![cfg(windows)]` at `:31`.

- **POSIX behavior**: Different mechanism. POSIX terminals deliver
  Shift+Enter as the same `\r` as plain Enter at the kernel layer —
  disambiguation is the terminal emulator's job (for example iTerm's
  "Send literal newline for Shift+Enter") and is out of `clud`'s scope.

### (e) Console title OSC keeper

- **Symptom**: In cmd.exe / Windows Terminal, the title bar otherwise reads
  `Command Prompt` or the path to cmd.exe. Worse, the backend
  (`claude.exe` / `codex.exe`) and any tool it invokes (`git`, `npm`) emit
  OSC 0/2 title-set escape sequences continuously, so even if `clud`
  stamps the title once at launch, the child immediately overwrites it.

- **Solution**: Two complementary defenses.
  1. `set_for_current_cwd()` calls `SetConsoleTitleW` once at launch with
     `clud <cwd-basename>` and records the desired title in a process-wide
     `OnceLock<Arc<Mutex<String>>>` cell.
  2. A daemon thread (`clud-title-keeper`) polls every ~750 ms; whenever
     `GetConsoleTitleW` reports drift from the desired value, it
     re-stamps via `SetConsoleTitleW`. `OnceLock` guarantees at most one
     keeper thread per process even if `keep_setting_in_background` is
     called multiple times.

  For PTY-mode launches (`--pty` / POSIX `clud loop`) the `OscTitleStripper`
  stream filter in the same file eats OSC 0/2 sequences from the child's
  output before they reach the terminal, so the keeper rarely fires.
  Numeric OSC bodies other than `0`/`2` (8 hyperlinks, 10/11 color queries,
  52 clipboard, 133 prompt marks, etc.) pass through verbatim — stripping
  them would break TUIs that rely on the response.

- **File**: `crates/clud-bin/src/console_title.rs:48`
  (`set_for_current_cwd`); keeper at `:70` (`keep_setting_in_background`)
  and `:76` (`spawn_keeper_thread`, Windows); `OscTitleStripper` at `:176`.

- **POSIX behavior**: No-op for the keeper half. `spawn_keeper_thread` at
  `:95` is an empty `#[cfg(not(windows))]` stub; `set_title` at `:158` is
  also a no-op. The `OscTitleStripper` is platform-agnostic because the
  PTY pump runs on every OS.

### (f) `IDropTarget` adapter for terminal drag-drop

- **Symptom**: Dragging a file onto a console window running `clud` on
  Windows produces the OS "no-drop" cursor; conhost rejects the drop at
  the OLE layer (`IDropTarget::DragEnter` → `DROPEFFECT_NONE`) so no bytes
  ever reach `clud`'s stdin (issue #65). Even when registration succeeds,
  Claude Code's backend later registers its own `IDropTarget` and
  displaces ours (issue #79).

- **Solution**: Spawn an STA (Single-Threaded-Apartment) worker thread
  that calls `OleInitialize` and `RegisterDragDrop` on
  `GetConsoleWindow()` — and on the top-level `WindowsTerminal.exe`
  window when present (under Windows Terminal `GetConsoleWindow()` returns
  a `PseudoConsoleWindow` and Explorer hovers over the terminal window
  instead). The thread waits a default 2 s initial delay so Claude Code
  registers first, then re-calls `RegisterDragDrop` every 3 s to displace
  any later re-registration. The IDropTarget callback parses the
  `CF_HDROP` payload via the panic-free `parse_dropfiles_buffer`,
  normalizes each path via `dnd::normalize_dropped_path`, and hands the
  list to a per-launch-mode `DropInjector`:
  - **Subprocess mode**: synthesizes Win32 `INPUT_RECORD` bytes (20-byte
    records, key-down + key-up per char, `VK_RETURN` for `\n`) into the
    console input buffer via `WriteConsoleInputW`.
  - **PTY mode**: writes the joined bytes (`\n`-separated paths plus a
    trailing space) into the PTY master so the slave's TTY reader sees
    them as if typed.

  The RAII guard (`ConsoleDropTargetGuard`) signals the worker, revokes
  each registered window, and calls `OleUninitialize` on the same STA
  thread when dropped — COM lifecycle has to stay on the thread that
  initialized it.

- **File**: `crates/clud-bin/src/dnd/console_drop_target.rs:384`
  (`register_console_drop_target`, Windows); `:392` (POSIX stub);
  `ConsoleDropTargetGuard` at `:333`; platform-agnostic dispatch at `:407`
  (`dispatch_dropfiles_to_injector`). Injectors at
  `crates/clud-bin/src/dnd/injectors.rs:71` (`build_input_records`),
  `:138` (`pty_master_injector`), `:157` (`subprocess_console_injector`,
  Windows only).

- **POSIX behavior**: Different mechanism. POSIX terminals deliver drops as
  stdin bytes (cmd-style quoted paths, mintty `/c/...` MSYS paths,
  PowerShell `& 'C:\...'`, macOS backslash-escaped spaces, GNOME
  `file://` URIs); the cross-platform `dnd::normalize_dropped_path` and
  `looks_like_dropped_path` string transforms in `dnd/mod.rs:49,77`
  handle those. The non-Windows stub of `register_console_drop_target`
  at `:392` returns `Err(RegisterError::UnsupportedPlatform)` so POSIX
  call sites simply no-op.

### (g) `CREATE_NO_WINDOW` for invisible helper spawns

- **Symptom**: When `clud` spawns daemon-helper / worker / repeat-job
  subprocesses with fully piped stdio on Windows, the OS allocates a
  brand-new conhost window for each child — each allocation is a visible
  flash that steals focus from the developer's window during the
  integration test suite, and from the user's window in production for
  `clud --detach` (issue #55).

- **Solution**: A single source-of-truth helper
  `invisible_helper_creationflags()` that returns
  `Some(CREATE_NO_WINDOW)` (`0x0800_0000`) on Windows and `None`
  elsewhere — exactly the shape `running_process_core::ProcessConfig::creationflags`
  expects. Daemon-side spawn sites OR this into their flags; the
  user-facing backend spawn intentionally does *not* — the user wants
  to see that child's output, and the inherited console means no new
  window is created anyway. A separate helper
  `user_facing_backend_creationflags()` returns
  `Some(CREATE_NEW_PROCESS_GROUP)` (`0x0000_0200`) for the interactive
  backend so the OS skips the child (and its descendants) when delivering
  console `CTRL_C_EVENT`. That keeps confusing Python tracebacks from the
  `nodejs-wheel` distribution out of clud's clean Ctrl+C output — clud
  is then responsible for tearing the child tree down, which it does via
  quirk (i).

- **File**: `crates/clud-bin/src/win_creation_flags.rs:40`
  (`invisible_helper_flags`); `:56` (`invisible_helper_creationflags`);
  `:88` (`new_process_group_flags`); `:108`
  (`user_facing_backend_creationflags`); the `CREATE_NO_WINDOW` literal
  anchored at `:25` and the `CREATE_NEW_PROCESS_GROUP` literal at `:35`.

- **POSIX behavior**: All four helpers return `0` / `None` on non-Windows.
  POSIX has no equivalent of `CREATE_NO_WINDOW` — there is no separate
  console window to suppress — and the foreground-process-group /
  `SIGINT` semantics already match what Windows is opting into with
  `CREATE_NEW_PROCESS_GROUP`. Returning `None` (rather than `Some(0)`)
  lets `running-process-core`'s "no override" short-circuit stay intact.

### (h) `whisper-rs` ARM carve-out

- **Symptom**: `whisper-rs-sys`'s vendored C++ source does not compile on
  `aarch64-pc-windows-msvc`. Shipping that dep unconditionally would
  break the entire Windows ARM build of `clud`.

- **Solution**: The `whisper-rs` dep is target-gated in `Cargo.toml`. In
  `voice/worker.rs` the `WhisperContextHandle` type alias resolves to
  `WhisperContext` on supported targets and to `()` on Windows ARM; the
  real `transcribe_audio` (which loads the model and runs the Whisper
  inference) is `#[cfg(not(all(target_arch = "aarch64", target_os = "windows")))]`,
  and a stub at `:153` returns a descriptive error explaining the
  platform limitation. The test-bypass path
  (`CLUD_VOICE_TEST_TRANSCRIPT`) is preserved on both branches so the F3
  state-machine tests still run on Windows ARM. Mic capture, cue
  playback, the F3 push-to-talk state machine, and downsampling all ship
  unchanged on every target — the carve-out is scoped to the
  transcription call only.

- **File**: `crates/clud-bin/src/voice/worker.rs:9` (target-gated
  `use whisper_rs`); `:18`–`:21` (`WhisperContextHandle` alias); `:80`
  (real `transcribe_audio`); `:153` (Windows ARM stub).

- **POSIX behavior**: Same as Windows x86_64 — the real `transcribe_audio`
  is compiled. Only `aarch64-pc-windows-msvc` is carved out; Linux ARM
  and macOS ARM build `whisper-rs` normally.

### (i) `process_tree::kill_tree` for Ctrl+C backend-tree reap

- **Symptom**: User hits Ctrl+C in a subprocess-mode backend session and
  the prompt takes several seconds to come back; in the meantime an
  orphaned `node.exe` (the real Claude/Codex backend) keeps writing
  garbage to the inherited console. The
  cause is structural: because of quirk (b) the real process tree at
  runtime is `clud.exe → cmd.exe → node.exe`, and `process.kill()` on
  the direct child reaps only the cmd.exe — the node.exe survives until
  clud itself exits and its Job Object closes.

- **Solution**: Walk the descendant tree with `sysinfo` (using
  `ProcessRefreshKind::nothing()` so the snapshot stays sub-second on
  Windows, where `System::new_all()` takes tens of seconds), then
  `kill_with(Signal::Kill)` + `process.kill()` every descendant
  deepest-first, ending with the root. The cooperative companion
  `try_break_group` calls
  `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)` so a well-behaved
  native agent with a `SetConsoleCtrlHandler` for `CTRL_BREAK_EVENT` can
  flush state during the short grace window before the hard kill follows.
  That cooperative break is skipped when the direct child is the BatBadBat
  `cmd.exe` wrapper from quirk (b): cmd's batch interpreter responds by
  printing `Terminate batch job (Y/N)?` and waiting on stdin. The hard
  `kill_tree` step still runs for both Claude and Codex, so the wrapper and
  backend descendants are reaped without prompting.

- **File**: `crates/clud-bin/src/process_tree.rs:46` (`kill_tree`); `:75`
  (`descendant_pids`); `should_cooperative_break`; `:113`
  (`try_break_group`).

- **POSIX behavior**: Same `kill_tree` code path runs (`sysinfo` is
  cross-platform; `Signal::Kill` is SIGKILL on Unix; `process.kill()`
  is a redundant follow-up there but a no-op). `try_break_group` is a
  no-op stub at `:125` — POSIX has no `CREATE_NEW_PROCESS_GROUP` concept
  and the terminal already delivers SIGINT to clud's foreground process
  group directly. The cross-platform test at `:202`
  (`kill_tree_terminates_real_descendant_on_unix`) spawns
  `sh -c 'sleep 30'` to mirror the `clud → cmd → child` shape and
  asserts the parent is reaped within 5 s.

### (j) Codex hook batch wrappers need `$LASTEXITCODE`

- **Symptom**: `clud --codex` prints:

  ```text
  [clud] warning: Codex hook command in ...\.codex\hooks.json uses a Windows batch wrapper without explicit `$LASTEXITCODE` propagation; a blocking hook may fail open.
  ```

  This is not a backend launch failure. It is a hook-health warning for Codex
  `PreToolUse` commands that mention `.cmd` or `.bat` without also mentioning
  `$LASTEXITCODE`. On Windows, many npm-installed tools are batch wrappers,
  and a PowerShell hook command that does not explicitly exit with the native
  command's last exit code can report success to Codex after the wrapped hook
  failed. For a blocking hook, that is a fail-open permission path.

- **Solution**: `hook_health::warn_on_powershell_exit_code_risk` scans Codex
  hook command strings during the `--codex` launch parity check. The diagnostic
  is warning-only; clud never edits `hooks.json` for this case because the safe
  repair depends on the user's hook command. The user should either call a
  native executable directly or make the PowerShell command end with
  `exit $LASTEXITCODE` after invoking the batch wrapper.

- **File**: `crates/clud-bin/src/hook_health/inspect.rs`
  (`warn_on_powershell_exit_code_risk`); launch gating at
  `hook_health/mod.rs` (`should_check_launch`). The unit coverage for hook
  parity lives in `crates/clud-bin/src/hook_health_tests.rs`.

- **POSIX behavior**: No-op. The scanner returns immediately unless
  `cfg!(target_os = "windows")` is true.

## Cross-cutting patterns

- **`#[cfg(windows)]` placement is module-local.** Each quirk lives in its
  own module; `cfg`-gating happens at the function, field, or impl
  boundary so call sites can call the public API unconditionally.
  `ConsoleVtGuard` returns the same type on every OS (the field is gated);
  `invisible_helper_creationflags` returns `Option<u32>` on every OS (the
  value, not the signature, is gated); `try_break_group` returns `bool`
  on every OS (the body is gated). The rest of the codebase never wraps
  its own call sites in a `cfg!` check.

- **RAII for console state.** Anything that mutates global console state
  returns a guard whose `Drop` impl restores the previous state:
  - `console_setup::ConsoleVtGuard` (`console_setup.rs:8`) restores the
    saved console-input mode.
  - `dnd::console_drop_target::ConsoleDropTargetGuard`
    (`dnd/console_drop_target.rs:333`) revokes each registered window
    and calls `OleUninitialize` on the same STA thread.
  - `trampoline::windows_stdio::NonInheritableStdioGuard`
    (`trampoline.rs:75`) restores `HANDLE_FLAG_INHERIT` on the three
    stdio handles after the detached spawn returns.

- **Single decision point for the `.cmd` rewrite.** `subprocess.rs` is the
  *only* file that knows about BatBadBat. Every backend spawn goes
  through `command_spec_for_subprocess` and gets the right `CommandSpec`
  variant back — nothing else has a special case for `.cmd` / `.bat`.

- **Single source of truth for `CREATE_NO_WINDOW`.** Every daemon-helper
  spawn imports from `win_creation_flags` rather than defining the
  `0x0800_0000` literal locally. The literal is anchored by
  `create_no_window_value_matches_winapi` at `win_creation_flags.rs:125`
  (Windows) and `invisible_helper_flags_is_zero_off_windows` at `:136`
  (POSIX) so a typo in either branch fails CI.

- **Best-effort, non-fatal startup.** The trampoline, the title keeper,
  the IDropTarget worker, and the `.old.*` GC are all wrapped so a
  failure logs to stderr (or stays silent) and the launch continues.
  None of them can block a `clud` invocation.

## Testing on non-Windows

Most of these modules are no-op stubs on POSIX, which means the Linux/macOS
unit-test runs cover only the dispatch logic, not the OS calls themselves.
Two exceptions worth flagging:

- `console_input::translate` (the Shift+Enter translator) is a pure
  function over a `&[InputEvent]` slice. Tests construct
  `InputEvent::Key { ... }` values directly and assert on the output
  `Vec<u8>`, so the full translation contract is unit-tested on every
  platform — see the seven `#[test]` cases in
  `crates/clud-bin/src/console_input.rs:97-198`.

- `console_title::OscTitleStripper` is also a pure byte filter and is
  fully unit-tested cross-platform — including split-across-chunks,
  back-to-back OSCs, and passthrough for OSC 8/10/52/133. See the test
  cases at `crates/clud-bin/src/console_title.rs:395-513`.

The other quirks (`trampoline::unlock_exe`,
`subprocess::command_spec_for_subprocess` Windows branch,
`console_setup::enable_console_vt_input`,
`dnd::console_drop_target::register_console_drop_target`,
`process_tree::kill_tree` Windows tree shape) have `#[cfg(windows)]` tests
that only run on the Windows x86 and ARM matrix jobs. The cross-platform
side of `process_tree::kill_tree` is covered by the Unix-only test at
`process_tree.rs:202` so the descendant-walk contract is enforced on every
host.

## See also

- [session-lifecycle.md](session-lifecycle.md) — how `console_setup`,
  `console_title`, and `console_input` feed into the PTY pump and the
  interactive-hooks loop.
- [../../crates/clud-bin/src/dnd/README.md](../../crates/clud-bin/src/dnd/README.md)
  — the drag-and-drop subsystem in detail, including the per-launch-mode
  injector contract and the `CF_HDROP` wire format.
- [../../crates/clud-bin/src/voice/README.md](../../crates/clud-bin/src/voice/README.md)
  — the F3 voice-mode pipeline, including the Windows ARM carve-out for
  transcription.
