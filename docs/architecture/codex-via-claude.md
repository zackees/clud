# Codex via Claude bridge

`clud --codex --harness claude` is a foreground-only compatibility bridge: it
binds an authenticated loopback Messages endpoint for the launched Claude
harness, translates to OpenAI Responses, and forwards to the selected Codex
credential route. Native Claude launches and every non-bridge route remain
unchanged; stopping the foreground runtime stops the listener and is the
rollback boundary.

## Model discovery and context

Claude Code 2.1.223 or newer discovers a Codex-only catalog from the bridge's
authenticated `GET /v1/models`. The three rows use reserved harness-facing
IDs (`clud-claude-codex-sol`, `clud-claude-codex-terra`, and
`clud-claude-codex-luna`); the bridge rewrites the selected row to its real
`gpt-5.6-*` wire ID before calling OpenAI. Unknown reserved IDs fail locally.
Provider wire IDs and the legacy `<model>@<effort>` spelling remain accepted
for continued sessions and forward-compatible explicit IDs, but clud no
longer emits a compound wire ID to Claude Code.

Ordinary effort travels through Claude Code's session effort field and reaches
the translator as `output_config.effort`. The provider-native `none` value,
which Claude Code's CLI does not accept, remains a suffix on the synthetic ID.
The child overlay enables gateway discovery, removes the retired scalar custom
picker row, and derives `CLAUDE_CODE_MAX_CONTEXT_TOKENS=1050000` from the
common context metadata on every advertised Codex catalog row so Claude Code
does not apply its unknown-model 200K compaction fallback. A future Codex row
with missing or different context metadata fails the catalog invariant rather
than silently inheriting this value. Setting
`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` is incompatible with this route
and fails before child launch.

## Conversation state and compaction

The bridge owns an in-memory canonical Responses transcript for its lifetime.
When Claude supplies `X-Claude-Code-Session-Id`, the main foreground turn is
keyed by that session and each `x-claude-code-agent-id` is isolated beneath the
same session. The identifiers are hashed before entering the map; the parent
agent header is provenance only and never selects history. Clients without
these headers use the bridge-session fallback identity. State is evicted when
the bridge stops and is bounded to 32 conversations, 16,384 items, and 64 MiB
per conversation; no transcript is persisted to disk.

A Messages request is a display/replay view, not the canonical transcript.
After a successful turn, the bridge appends only that logical turn's newly
pending input, followed by verbatim `response.output_item.done` output. It
does not re-append the historical `messages` array that the harness resends on
every call. Server output is retained as opaque JSON, preserving generated
item IDs, encrypted reasoning content, and forward-compatible unknown fields.
The pending input is the complete Messages suffix after the final assistant
turn, not merely `messages.last()`: Claude Code may represent one parallel tool
batch as consecutive user messages with one `tool_result` apiece. A terminal
assistant prefill remains pending for compatibility. Before an
inference request reaches upstream, the bridge verifies that the assembled
canonical input has an output for every function call. A mismatch returns a
local 400 and records only conversation scope plus fixed item kinds/counts in
the failure log; call IDs, tool payloads, Messages content, and raw Claude
identity headers remain absent.
Failed or partial turns are never committed. If a completed turn's input and
output cannot fit the per-conversation item or byte limit, the client still
receives that completed reply; only after its downstream response is committed,
the bridge atomically clears that conversation's canonical transcript. The next
replay therefore seeds fresh canonical history rather than continuing stale
items.

When a first inference attempt fails with the exact
`context_length_exceeded` code **before any Anthropic-visible text, reasoning,
or tool-call frame**, the bridge performs one bounded recovery cycle. The code
is accepted from either a non-2xx JSON error envelope or an HTTP 200
`response.failed` SSE event; status codes and free-form messages are not
signals. The bridge sends only prior canonical history to `/responses/compact`,
validates the opaque compaction output, atomically replaces canonical history,
appends the pending current input exactly once, and retries inference once.
`response.created` is only a protocol envelope and does not count as visible
output. No other error code recovers; a compact error, malformed compact
output, cancellation, a second context-full response, or any output before
failure stays terminal. Recovery suppresses every frame from the failed
attempt, so the client receives only the retry's single valid response
sequence.

The authenticated loopback listener also exposes three launch-private lifecycle
controls. Every bridged Claude launch registers session-local HTTP hooks through
a protected temporary `--settings` file: `PreCompact` (manual or automatic)
calls compact before Claude mutates its transcript, and `SessionStart(clear)`
clears the bridge after Claude starts the fresh session:

- `POST /_clud/context/compact` compacts the bridge's canonical transcript and
  replaces it only after a valid opaque response. Empty history makes no
  provider request but arms the same post-compaction reset. If the transcript
  has an outstanding function call or the selected
  credential route cannot compact it, the bridge acknowledges the hook and
  enters a harness-compaction fallback instead of blocking Claude.
- `POST /_clud/context/compact-finished` handles `SessionStart(compact)`. It
  completes a pending fallback by discarding the pre-compaction transcript that
  Claude's summary inference temporarily replayed. The next ordinary turn then
  seeds canonical history from Claude's compacted transcript. When provider-side
  compaction succeeded, this control is a no-op and preserves its opaque output.
- `POST /_clud/context/clear` clears the addressed Claude session and all of its
  agent descendants, then performs no upstream inference or compaction request.

All three routes use the same loopback URL and launch-scoped
`ANTHROPIC_AUTH_TOKEN` already supplied to the child. The generated settings
interpolate the bearer from the environment, so it never appears in argv.
Routes accept either an empty direct-control body or the exact matching Claude
lifecycle JSON, serialize with normal turns, and return `204` on success. They
are not part of the public Anthropic Messages surface.
