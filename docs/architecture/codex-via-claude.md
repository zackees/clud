# Codex via Claude bridge

`clud --codex --harness claude` is a foreground-only compatibility bridge: it
binds an authenticated loopback Messages endpoint for the launched Claude
harness, translates to OpenAI Responses, and forwards to the selected Codex
credential route. Native Claude launches and every non-bridge route remain
unchanged; stopping the foreground runtime stops the listener and is the
rollback boundary.

## Conversation state and compaction

The bridge owns an in-memory canonical Responses transcript for its lifetime.
It is keyed by the bridge-session fallback identity because the current Messages
surface supplies no conversation identifier. State is evicted when the bridge
stops and is bounded to 32 conversations, 16,384 items, and 64 MiB per
conversation; no transcript is persisted to disk.

A Messages request is a display/replay view, not the canonical transcript.
After a successful turn, the bridge appends only that logical turn's newly
pending input, followed by verbatim `response.output_item.done` output. It
does not re-append the historical `messages` array that the harness resends on
every call. Server output is retained as opaque JSON, preserving generated
item IDs, encrypted reasoning content, and forward-compatible unknown fields.
Failed or partial turns are never committed. If a completed turn's input and
output cannot fit the per-conversation item or byte limit, the client still
receives that completed reply; only after its downstream response is committed,
the bridge atomically clears that conversation's canonical transcript. The next
replay therefore seeds fresh canonical history rather than continuing stale
items.

When a first inference attempt fails in-band with the exact
`context_length_exceeded` code **before any Anthropic-visible text, reasoning,
or tool-call frame**, the bridge performs one bounded recovery cycle: it sends
only prior canonical history to `/responses/compact`, validates the opaque
compaction output, atomically replaces canonical history, appends the pending
current input exactly once, and retries inference once. `response.created` is
only a protocol envelope and does not count as visible output. No other error
code recovers; a compact error, malformed compact output, cancellation, a
second context-full response, or any output before failure stays terminal.
Recovery suppresses every frame from the failed attempt, so the client receives
only the retry's single valid response sequence.
