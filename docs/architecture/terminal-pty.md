# Terminal and PTY Byte Path

clud sits between the user's terminal and a child TUI (`claude` or `codex`)
running in a PTY, and every byte in both directions passes through clud's
pump. This doc owns the **cross-platform** facts about that byte path: what
transforms each direction, the invariants every change must keep, how to
test it, and how to validate it. Thread structure, startup order and
shutdown are in [session-lifecycle.md](session-lifecycle.md); Windows console
plumbing is in [windows-quirks.md](windows-quirks.md). Platform playbooks
are listed under [Platform skills](#platform-skills).

## Data flow

### Input: user terminal to child

```
user terminal
  ├─ Windows, interactive console: console_input.rs native ReadConsoleInputW
  │    reader → one chunk per translated key event → extra_rx
  └─ POSIX, piped stdin, or no native reader: byte-stream stdin reader
        │
        ▼
InterruptScanner::requests_interrupt   (one per input stream, #1703)
        │
        ▼
forward_user_input                      (session.rs:1252)
  └─ filter_user_input_chunk            (session.rs:1197)
       ├─ BracketedPasteNormalizer      (owns EscapeSequenceGate, #1697)
       └─ toast MouseFilter             (only while a toast is armed, #1189)
  └─ process.write_impl → PTY
  └─ F3Observer (voice hotkey, observes the chunk, never consumes it)
```

- `InterruptScanner` (`session_stdin.rs:65`) flags a `0x03` byte or a kitty
  CSI u Ctrl+C (`\x1b[99;5u` and its `:1`/`:2` spellings). Each stream keeps
  its own scanner so a CSI split across two reads counts exactly once
  (#1703). The pump checks it before forwarding, then forwards the chunk,
  then calls `interrupt_pty_process`.
- `BracketedPasteNormalizer` (`session/bracketed_paste.rs:32`) rewrites
  dropped-path pastes and passes its output through `EscapeSequenceGate`
  (`session/escape_gate.rs:19`).
- `MouseFilter` (`toast/mouse.rs:16`) swallows a click on the toast close
  button; clud never turns mouse tracking on itself.
- Daemon attach mirrors this with `RemoteInputFilter`
  (`daemon/attach_input.rs:47`), which owns its own paste normalizer, F3
  observer and `InterruptScanner`.

### Output: child to user terminal

```
PTY → reader thread
        ├─ OscTitleStripper            (drops OSC 0/2 title writes)
        ├─ CodexLfNormalizer           (Codex only: bare LF → CRLF, #1181)
        ├─ KeyboardEnhancementTracker::observe
        │     kitty push/pop frames (#1221) + ChildModes (#1704)
        └─ TerminalQueryScanner        (only when no interactive console
                                        answers: #1310/#1347/#1702)
      → unbounded channel → writer thread
        └─ toast compositor (draws only when the stream is at ground state)
      → user terminal
```

- `KeyboardEnhancementTracker` (`session.rs:344`) counts the child's
  `CSI > … u` pushes and `CSI < … u` pops, and feeds the same bytes to
  `ChildModes` (`session/child_modes.rs:179`), a `vte` parser that records
  which terminal modes the child left on.
- `TerminalQueryScanner` (`terminal_queries.rs:35`) stub-answers `ESC[6n`,
  DA1/DA2, kitty `?u` and OSC 10/11 queries. The local pump only answers when
  `should_answer_cursor_queries` (`session.rs:1084`) is true: on Windows, when
  stdin is not an interactive console. With a real console the real terminal
  answers, and a stub would be a second, wrong reply.
- `OscTitleStripper` lives in `console_title_osc.rs:3`; `CodexLfNormalizer`
  in `codex_lf.rs:24`; the compositor writer in `session_output.rs:38`.
- Daemon attach relays output through the same tracker and stripper
  (`daemon/attach.rs:387`, `daemon/attach.rs:512`). The worker answers
  queries only while no client is attached (`detached_query_replies`,
  `daemon/worker.rs:669`).

## Invariants

### 1. Sequence atomicity on PTY input

Never write a partial escape sequence to the PTY. ConPTY's input parser
treats the end of each write as the end of input and flushes whatever
sequence is still open. In #1697 an SGR mouse report written one character
at a time lost its `ESC [ <` and reached the child as typed text
`35;31;18M`.

`EscapeSequenceGate` holds an incomplete tail (a lone ESC, `ESC O`, or
`ESC [` followed only by parameter/intermediate bytes, at most 64 bytes)
until its final byte arrives. Held bytes are released after an idle gap that
depends on what is held (`BracketedPasteNormalizer::flush_due_in`): a lone
`ESC`, `ESC [` or `ESC O` (what an Esc or Alt+[ / Alt+O keypress sends) after
5 ms, so those keys never lag, and anything longer (a terminal report or a
partial `ESC[200~`) only after 250 ms. Releasing a partial report after 5 ms
wrote it on its own and ConPTY dropped it whenever input arrived in pieces
more than 5 ms apart (#1717). The rule is enforced on every platform, not only Windows: any
new path that calls `write_impl` with user input must go through
`forward_user_input`, or through an equivalent that keeps sequences whole.

### 2. Every stream scanner resumes across chunk boundaries

PTY reads and console events split sequences at arbitrary offsets. Every
scanner over a byte stream must carry state between calls. Current examples
that do: `OscTitleStripper`, `CodexLfNormalizer`,
`KeyboardEnhancementTracker`, `F3Observer`, `MouseFilter`,
`TerminalQueryScanner`, `InterruptScanner`, and `ChildModes` (through
`vte`). A stateless per-chunk scan is a known bug class. Before #1702 the
pump matched queries only within one chunk; before #1703 a CSI u Ctrl+C
split across reads was missed.

### 3. Restore the terminal on exit

Ctrl+C is an immediate tree kill (`interrupt_pty_process`,
`session/interrupt.rs:26`), so the child's own exit path never runs. The
session must undo the child's terminal state itself (#1701, #1704).
`RawTerminalGuard::drop` (`session.rs:602`) runs after the pump has joined
its reader, so every child byte has been observed. It writes, in order:

1. The `ChildModes` reset, which turns off exactly what is still on. The
   alternate screen is left first. The scroll-region reset is wrapped in
   cursor save/restore (`ESC 7 … ESC [ r … ESC 8`), because `CSI r` homes the
   cursor. `?1049l` is never sent unless the child entered `?1049`.
2. One pop for each kitty frame the child pushed and did not pop.
3. `CHILD_TERMINAL_MODES_RESET` (`session.rs:453`): all mouse modes,
   `?1004`, `?2004`, and `?25h`.
4. clud's own kitty frame is popped, then raw mode is left.

Forced kills (`kill -9`, `TerminateProcess`) run no code in the process and
are out of scope. The out-of-process guard process was removed
([DD-147](../DESIGN_DECISIONS.md#dd-147-ctrlc-restores-the-terminal-in-process-forced-kills-are-out-of-scope),
which supersedes DD-146). The full restore sequence is described under
[session-lifecycle.md § Terminal restore](session-lifecycle.md#terminal-restore).

## Testing

Use the lowest layer that can observe the behaviour. The general tiers are
in [testing-tiers.md](testing-tiers.md).

- **Pure byte transforms.** Unit tests live in
  `crates/clud-bin/src/session_tests.rs` and in each module's `tests`
  module. Any stream parser should have a test that splits the input at
  every offset, for example
  `every_split_of_mixed_input_is_byte_exact_and_never_ends_mid_sequence`
  (`session_tests.rs:399`),
  `csi_u_ctrl_c_split_at_every_offset_requests_exactly_one_interrupt`
  (`session_tests.rs:745`), and `sequences_split_across_reads_are_followed`
  in `session/child_modes.rs`.
- **Real PTY harness.** `crates/clud-bin/tests/integration/pty/` drives real PTYs with
  [`mock-agent`](../../testbins/mock-agent/README.md):
  `--mock-ansi-script` (emit a byte file), `--mock-read-stdin-ms` and
  `--mock-stdin-raw-to` (capture exactly what reached the child), and
  `--mock-ready-file`. Examples include
  `a_ctrl_c_interrupted_child_has_its_terminal_modes_undone`
  (`tests/integration/pty/pty_pump.rs:378`) and
  `stdin_forwarding_stays_fast_while_output_sink_stalls`
  (`tests/integration/pty/pty_pump.rs:1051`). The pump has a test entry,
  `run_raw_pty_pump_with_extras` (`session.rs:855`).
- **Real console required.** `require_pty_or_skip!`
  (`crates/clud-bin/tests/integration/common/mod.rs:369`) skips a test when no real
  terminal is present, unless `CLUD_REQUIRE_PTY=1` is set. In that case it
  fails. CI's `ci/run_bundle.py` runs each `pty::` test of the `integration`
  harness in its own pseudo-terminal (`ci/harness_plan.py`,
  `TERMINAL_CATEGORY = "pty"`; `run_terminal_test`) and sets
  `CLUD_REQUIRE_PTY=1`, so a skip there becomes a red test.

## Validation

**Local (agents).** Run tests and lint only through `bosn ci`, which replays
the workflow under act2 in an isolated engine
([ci.md § Local validation](ci.md#local-validation-before-remote-ci)):

```bash
bosn ci run --workspace . --trigger pr --job static --wait  # fmt, ruff, static checks
bosn ci run --workspace . --trigger pr --wait               # adds Linux clippy, build, Rust + Python unit suites
```

Never run host `cargo`, `bash lint` or `bash test`. act cannot run Windows
or macOS. Each run snapshots this checkout into its own engine, so it cannot
test another checkout's tree, and the old act-only false failures are fixed:
the wrapper's log test is gone with the wrapper, and the six
`tests/test_codex_installer_rm.py` cases were a real symlinked-alias bug in
the rm guard (#1746).

**Remote.** See [ci.md § Current CI selection](ci.md#current-ci-selection).

| PR label | Adds |
|---|---|
| *(none)* | Static checks, Linux x64 build + unit, and Dylint (which also compiles check-only for `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`, so it catches macOS-only compile errors) |
| `ci-windows` | Static checks plus the Windows x64 build, unit and integration lanes (fast Windows iteration) |
| `ci-test` | Linux x64 integration plus Windows x64 |
| `ci-full` | All six targets, including hosted macOS arm64 and x64 |

- Watch a PR with `clud tool run github/pr_merge_watch.py -- <PR> [--no-cancel]`.
  Hand-written polling loops are blocked by a hook.
- Read a job log with
  `gh api --allow-escape-sequences repos/zackees/clud/actions/jobs/<job_id>/logs`.

## Platform skills

Repo-local playbooks for an agent about to change or debug this path on one
platform. They link back here for the shared facts above.

- [terminal-pty-windows](../../.claude/skills/terminal-pty-windows/SKILL.md):
  ConPTY, the native console reader, VT input, and what ConPTY does and
  does not forward.
- [terminal-pty-linux](../../.claude/skills/terminal-pty-linux/SKILL.md):
  POSIX PTY, raw mode and OPOST, signals, and full local validation via act.
- [terminal-pty-macos](../../.claude/skills/terminal-pty-macos/SKILL.md):
  libc type differences and `ci-full`-only validation.

## See also

- [session-lifecycle.md](session-lifecycle.md): pump threads, startup and
  shutdown order, and terminal restore.
- [windows-quirks.md](windows-quirks.md): VT-input RAII and the native
  console reader.
- [toasts.md](toasts.md): the compositor and mouse filter.
- [daemon-ipc.md](daemon-ipc.md): the attach flow.
