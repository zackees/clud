# mock-agent/src/

Source for the `mock-agent` binary (crate `mock-agent`, see
`../Cargo.toml`). Integration tests copy or symlink this binary onto `PATH`
under the name `claude` or `codex` so the `clud` CLI launches it instead of a
real agent. The binary parses its own `--mock-*` flags out of `argv`, records
the remaining (forwarded) args, optionally reads stdin, optionally writes
marker files / probe data, and then emits a single JSON report on stdout
before exiting with the test-requested code.

## Files

- `serve.rs` — `mock-agent serve`: the scripted Anthropic Messages backend for
  the real-harness tier (see "Server mode" below and
  [testing-tiers.md](../../../docs/architecture/testing-tiers.md)).
- `main.rs` — Entire mock-agent implementation: arg filtering, stdin capture
  (timed + pipe modes, raw-mode on Unix TTYs), iteration counter for
  `clud loop` marker tests, helper-process tree spawning, terminal-size
  polling, scripted ANSI/stream-json emission, and the final JSON report.

## Behavior

- Reads argv. Recognized `--mock-*` flags are consumed; everything else is
  echoed back in the report's `args` field exactly as `clud` forwarded it.
- A leading `--version` arg short-circuits to a Claude Code version line
  (issue #921's unified gateway-discovery gate): prints `MOCK_CLAUDE_VERSION`
  if set, else `9.9.9 (mock-agent)`, then exits 0 without the JSON report.
- Recognized flags (each takes the next argv slot as its value):
  - `--mock-exit-code <n>` — exit with `n` (default 0).
  - `--mock-sleep-ms <ms>` — sleep before emitting the JSON report.
  - `--mock-read-stdin-ms <ms>` — read stdin for up to N ms even on a TTY;
    puts the TTY into raw mode on Unix so non-newline bytes flush.
  - `--mock-stdin-raw-to <path>` — also dump captured stdin bytes to a file.
  - `--mock-report-file <path>` — duplicate the JSON report to a file (useful
    when stdout is owned by a PTY).
  - `--mock-ready-file <path>` — with `--mock-read-stdin-ms`, atomically
    write `{"pid", "stdin_raw_ready": true}` once the stdin mode is final, so
    a test sends input only after ConPTY will preserve it (#1310).
  - `--mock-started-file <path>` — atomically publish early PID and Kitty pane
    environment JSON before waiting or producing the final report.
  - `--mock-wait-for-file <path>` — hold the process until the named file
    appears (120-second safety limit), keeping a seed pane alive for reuse.
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
  - `--mock-tool-shell-probe <path>` — Windows-only #616 subprocess probe.
    The mock agent (copied as `codex.exe`) launches a PowerShell tool root
    which leaves a sleeping client behind, then writes whether clud's
    foreground Job tracker reaped that client.
  - `--mock-codex-bridge-probe <path>` — issue #626 bridge probe. Reads the
    child-only Anthropic URL/token, sends the embedded deterministic Messages
    request, and records only sanitized loopback/status/fixture observations
    plus the ephemeral port (never the URL or bearer).
  - `--mock-codex-cache-identity-probe <path>` - issue #1226's subprocess-only
    eight-turn main/agent bridge probe with a fixed large prefix. Records only
    status and reply counts;
    the Python fake upstream owns the allowlisted wire assertions.
  - `--mock-stream-json <path>` with `--mock-stream-delay-ms <ms>` - emit one
    pre-canned `--output-format stream-json` line per file line, flushing
    between each, then exit (no JSON report tail).
- Env vars `IN_CLUD` and `RUNNING_PROCESS_ORIGINATOR` are captured and
  included under `env` in the report; #626 bridge variables are recorded as
  presence booleans and non-secret tuning values only; current working
  directory is captured as `cwd`.
- Stdout: the JSON report (one line) unless a mode above short-circuits
  (`--mock-stream-json`, `--mock-report-pty-size`, helper role). Scripted ANSI
  or stream-json bytes are emitted before the report when configured.
- Stderr: unused.
- Exit code: value of `--mock-exit-code`, default 0.

## Server mode: `mock-agent serve`

```
mock-agent serve --script script.json [--port 0] [--log requests.jsonl] [--port-file port.txt]
```

It binds to `127.0.0.1` and prints `listening <port>` (and writes `--port-file`).
It serves `POST /v1/messages` (SSE when `"stream": true`, JSON otherwise),
`POST …/count_tokens` and `GET /v1/models`. Point Claude Code at it with
`ANTHROPIC_BASE_URL`.

Script format:

```json
{
  "default_text": "DONE",
  "roles": [
    {"name": "worker", "match": "You are a /grind worker", "steps": [
      {"tool_use": {"name": "Bash", "input": {"command": "cargo build"}}},
      {"expect": {"is_error": true, "content_contains": "BLOCKED"}, "text": "denied as expected"}
    ]},
    {"name": "main", "steps": [
      {"structured": {"answer": 42}},
      {"error": {"status": 500}}
    ]}
  ]
}
```

- **Role:** the first role whose `match` substring is in the request's system
  prompt, and whose optional `match_prompt` (a string, or a list that must all
  appear) is in its messages; a role with no `match` is the fallback.
  `match_prompt` tells apart several agents of one role, such as one `/grind`
  planner per goal (`"Goal g1:"`).
- **Step:** the number of assistant turns already in the request's messages.
  Past the last step, or when a step's tool isn't offered in the request, the
  reply is `default_text`.
- **Step kinds:** `tool_use {name, input}`, `structured {…}` (a
  `StructuredOutput` call), `text`, and `error {status}` (an HTTP error reply).
- **`delay_ms`** on any step holds the reply open, so concurrent agents
  visibly overlap in the log's `t_start` / `t_end` (epoch milliseconds).
- **`expect`** checks the `tool_result`s sent back for the previous step
  (`is_error`, `content_contains`). A mismatch replies
  `MOCK_EXPECT_FAILED: …` and is recorded in the log's `note`.
- **Log:** one JSON line per request, with `n`, `role`, `turn`, `step`,
  `note`, `t_start`, `t_end`, `tools`, `tool_results`, `system` and
  `messages`.
