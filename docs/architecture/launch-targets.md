# Launch Targets and Sticky Preferences

clud resolves two independent launch dimensions before harness bootstrap,
setup, or launch-plan construction:

- `ModelProvider` is the API/model family selected by `--claude` or `--codex`.
- `HarnessSelection` is `default`, `claude`, or `codex`, selected by
  `--harness`.

`default` maps the provider to its native harness: Claude to Claude and Codex
to Codex. The resolved `Backend` remains the concrete executable compatibility
type used by bootstrap, setup, argv construction, and runners. New code must
not infer the provider from that legacy field; use `ResolvedLaunchTarget` or
`LaunchPlan::model_provider()`.

## Resolution

`backend::resolve_launch_target` is the single precedence and validation
function. Each dimension independently uses:

```text
explicit CLI > ~/.clud/settings.json > built-in default
```

The built-in provider is Claude and the built-in requested harness is
`default`. Each resolved value carries a `PreferenceSource`: `cli`,
`global_setting`, or `built_in_default`.

The supported cross-route in phase 1 is Codex provider through Claude:

```text
clud --codex --harness claude
```

Claude provider through Codex is rejected before bootstrap with:

```text
unsupported launch target: Claude provider cannot use the Codex harness
```

There is no fallback. HTTP translation and credentials are later phases under
issue #622.

## Foreground Codex-through-Claude bridge

Issue #626 gives the supported cross-route a foreground-only runtime shell.
`ForegroundRuntime` starts one authenticated bridge before the first harness
spawn, supplies its child-local environment to both `ManagedSubprocess` and
`NativePtyProcess`, and owns the bridge until the whole foreground runner
returns. Native provider/harness pairs retain their original environment
unchanged. The same bridge spans every in-process foreground iteration; only
daemon, detached, and repeat-worker ownership remain later #622 phases.

The cross-route overlay replaces `ANTHROPIC_BASE_URL` and
`ANTHROPIC_AUTH_TOKEN`, removes ambient `ANTHROPIC_API_KEY`, and adds Claude's
long request timeout/nonessential-traffic tuning only when the user has not set
those values. No parent-process environment is mutated. The bearer and complete
base URL are deliberately absent from Debug/error/report surfaces.

`codex_bridge.rs` binds only `127.0.0.1:0` and implements the phase-2 routing
shell: authenticated `POST`/`HEAD /v1/messages`, an explicit unsupported 404
for token counting, and 404 elsewhere. Header/body sizes and worker concurrency
are bounded; handle Drop signals shutdown and joins the listener plus admitted
workers. See DD-027 for why this parser uses `std::net` instead of the
repository's existing `tiny_http` dependency.

Each I/O phase carries its own budget rather than sharing one connection
deadline: `header_timeout` (5 s) and `body_timeout` (30 s) are absolute, while
`stream_idle_timeout` (300 s) is re-armed per frame so a long model turn is
bounded by silence, not by elapsed time. Streamed replies go through
`write_event_stream`, which uses chunked transfer encoding and flushes one SSE
event at a time; `write_response` remains the `Content-Length` writer for
errors, `HEAD`, and non-streaming bodies. Accepted sockets are explicitly put
back into blocking mode, and a read timeout is fatal only once the phase
deadline has passed. See DD-028 — the previous single-deadline arrangement
answered `408` to any request whose body arrived in a later TCP segment.

## Request translation

`codex_translate.rs` maps an Anthropic Messages request onto an OpenAI
Responses request (#627 step 2). It is a pure function over typed structs with
no HTTP, socket, or credential dependency, so the mapping table can be driven
directly by fixtures; step 5 wires it into the bridge.

The shape difference that drives the design: one Messages `content` array can
become several Responses input items. An assistant turn holding text plus two
`tool_use` blocks becomes one message item and two `function_call` items, and
`tool_result` blocks become top-level `function_call_output` items rather than
message content. Order is preserved because the model reads the result as a
transcript.

Notable mappings: `system` → `instructions`; `input_schema` → `parameters`;
`tool_choice: any` → `required`; `disable_parallel_tool_use` (an opt-out) →
`parallel_tool_calls: false` (an opt-in), sent only when asked; `max_tokens` →
`max_output_tokens`; `thinking.budget_tokens` → the coarse `reasoning.effort`
ladder. A `claude*` model id resolves to the Codex default, while any other id
is honoured verbatim as an explicit override.

Unknown *top-level* request fields are tolerated so an additive Anthropic API
change cannot break the route, but anything that changes meaning and has no
faithful equivalent — `top_k`, `stop_sequences`, thinking blocks replayed in
history, images inside a `tool_result`, unknown block or `tool_choice` types —
is an explicit error rather than a silent drop.

## Response streaming

`codex_sse.rs` translates the upstream Responses SSE stream into Anthropic SSE
(#627 step 3), in two separable layers so fragmentation can be tested apart
from event semantics:

- `FrameDecoder` is byte-level. Line terminators are normalised on ingest (the
  SSE spec treats CRLF, LF, and bare CR alike), and a `\r` at the end of a read
  is held back so a CRLF split across two segments is not misread as CR plus a
  blank line. Comments/heartbeats are skipped, multiple `data:` lines join with
  a newline, and `finish()` flushes a final frame that arrived without a
  trailing blank line.
- `StreamTranslator` is semantic. It allocates Anthropic content-block indices
  monotonically in the order blocks open, keyed off the upstream
  `(output_index, content_index)`, so interleaved text and parallel tool calls
  each address their own block.

The invariant that shapes the design: **never emit a malformed Anthropic
block.** A `content_block_start` for a tool call cannot be sent until the
call's id and name are both known, so argument deltas that arrive first are
buffered and flushed once the block is legally open. A half-formed tool block
is worse than a late one — it makes the client fail to parse a turn it could
otherwise have used.

Termination is guaranteed: a truncated or disconnected stream still closes
every open block and emits `message_delta`/`message_stop`. Upstream failures
close open blocks and then emit a **sanitized** error — the upstream body is
never echoed, since it can carry account identifiers or key fragments and this
frame is written straight to the harness.

Reasoning summaries are deliberately not forwarded: an Anthropic `thinking`
block carries a signature the Responses API does not supply, and an unsigned
one is rejected by clients.

## Persistence

Global launch preferences use the existing settings document and `fs4` lock:

```json
{
  "backend": { "default": "codex" },
  "harness": { "default": "claude" }
}
```

`GlobalSettingsPatch` applies model, harness, and settings-TUI changes inside
one locked read/modify/write. The settings TUI uses one typed choice state
machine for the provider and harness rows. The same generic `ChoiceSelector`
drives the inline session/global launch-scope selector.

An interactive explicit provider or harness selection offers:

- **Session only**: use it for this invocation without writing settings.
- **Globally**: atomically persist the explicitly selected dimensions and the
  effective harness's global setup scope.

When a saved non-`default` harness affects a real TTY launch, clud prints one
green stderr line before spawning:

```text
[clud] Harness override: Claude (global setting)
```

It is suppressed for non-TTY stderr and structured output. `--dry-run` reports
the effective values and their sources instead.

## LaunchPlan and compatibility

Every newly built `LaunchPlan` carries `model_provider`, `requested_harness`,
`effective_harness`, `provider_source`, and `harness_source`. These are
`serde(default)` options because old daemon/worker payloads contain only
`backend`. Accessors fall back to that legacy field, preserving old native
behavior.

The daemon worker receives the same plan. Repeat jobs also pin the resolved
provider and requested harness in their one-shot argv so a settings edit cannot
silently change an existing job.

`backend` remains in dry-run JSON for compatibility and names the effective
executable. The additive fields make both dimensions explicit:

```json
{
  "backend": "claude",
  "model_provider": "codex",
  "requested_harness": "claude",
  "effective_harness": "claude",
  "provider_source": "cli",
  "harness_source": "cli"
}
```

## Ownership

- `backend.rs`: typed dimensions, precedence, validation, notice policy.
- `args.rs`: `--harness` parsing and passthrough ownership.
- `clud_settings.rs`: locked reads and atomic typed patches.
- `preference.rs`: shared choice state machine.
- `launch_setup.rs` / `settings_tui.rs`: session/global and settings UI.
- `command/types.rs` / `command/builder.rs`: plan metadata and harness argv.
- `main.rs`: bootstrap/setup the effective harness and emit dry-run metadata.
- `codex_bridge.rs` / `foreground_runtime.rs`: phase-2 foreground transport,
  child overlay, spawn seam, and lifetime ownership.
- `daemon/entry.rs`: pin repeat-job provider/harness choices.
