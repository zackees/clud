# mock-agent/src/

Source for the `mock-agent` binary (crate `mock-agent`, see
`../Cargo.toml`). Integration tests copy or symlink this binary onto `PATH`
under the name `claude` or `codex` so the `clud` CLI launches it instead of a
real agent. The binary parses its own `--mock-*` flags out of `argv`, records
the remaining (forwarded) args, optionally reads stdin, optionally writes
marker files / probe data, and then emits a single JSON report on stdout
before exiting with the test-requested code.

## Files

- `main.rs` — Entire mock-agent implementation: arg filtering, stdin capture
  (timed + pipe modes, raw-mode on Unix TTYs), iteration counter for
  `clud loop` marker tests, helper-process tree spawning, terminal-size
  polling, scripted ANSI/stream-json emission, and the final JSON report.

## Behavior

- Reads argv. Recognized `--mock-*` flags are consumed; everything else is
  echoed back in the report's `args` field exactly as `clud` forwarded it.
- Recognized flags (each takes the next argv slot as its value):
  - `--mock-exit-code <n>` — exit with `n` (default 0).
  - `--mock-sleep-ms <ms>` — sleep before emitting the JSON report.
  - `--mock-read-stdin-ms <ms>` — read stdin for up to N ms even on a TTY;
    puts the TTY into raw mode on Unix so non-newline bytes flush.
  - `--mock-stdin-raw-to <path>` — also dump captured stdin bytes to a file.
  - `--mock-report-file <path>` — duplicate the JSON report to a file (useful
    when stdout is owned by a PTY).
  - `--mock-write-done <path>` / `--mock-write-done-body <s>`,
    `--mock-write-blocked <path>` / `--mock-write-blocked-body <s>`,
    `--mock-write-marker-on-iter <n>` — `clud loop` DONE/BLOCKED contract:
    bumps an `iter-count` file in the marker's parent dir on each invocation
    and writes the marker once iteration `>= n`.
  - `--mock-helper-role <root|child|grandchild>` +
    `--mock-spawn-tree-log <path>` — process-tree tests; the root logs itself
    and spawns a detached child which spawns a grandchild.
  - `--mock-report-pty-size <path>` with `--mock-pty-size-samples <n>` and
    `--mock-pty-size-interval-ms <ms>` — poll `terminal_size` N times,
    write the samples as JSON, and print one `PTY_SIZE_SAMPLE i {json}` line
    per sample to stdout so the harness can resize between samples.
  - `--mock-ansi-script <path>` — write raw bytes from the file to stdout
    first (used by attach-replay tests).
  - `--mock-stream-json <path>` with `--mock-stream-delay-ms <ms>` — emit one
    pre-canned `--output-format stream-json` line per file line, flushing
    between each, then exit (no JSON report tail).
- Env vars `IN_CLUD` and `RUNNING_PROCESS_ORIGINATOR` are captured and
  included under `env` in the report; current working directory is captured
  as `cwd`.
- Stdout: the JSON report (one line) unless a mode above short-circuits
  (`--mock-stream-json`, `--mock-report-pty-size`, helper role). Scripted ANSI
  or stream-json bytes are emitted before the report when configured.
- Stderr: unused.
- Exit code: value of `--mock-exit-code`, default 0.
