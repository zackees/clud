# Codex-via-Claude

`clud --codex --harness claude` runs a Codex model through the Claude Code
harness. This is an opt-in cross-route: native `clud`, `clud --claude`, and
`clud --codex` launches retain their existing behavior.

## Resolution and ownership

Provider and harness resolve independently with `CLI > global setting >
built-in default`. `ModelProvider::Codex` plus `HarnessSelection::Claude` is
the supported cross-route; Claude through the Codex harness is rejected before
bootstrap. `LaunchPlan` carries the resolved provider, requested/effective
harness, and sources as additive optional fields. Older daemon payloads decode
them as absent and fall back to legacy `backend`, while repeat workers pin the
resolved choices so a later settings edit cannot change an existing job. See
[launch-plan.md](launch-plan.md) and [daemon-ipc.md](daemon-ipc.md).

The foreground runtime owns one bridge for the complete foreground runner,
including in-process iterations. It installs only child environment variables;
the parent environment is never changed. Daemon/detached/repeat workers receive
the same serialized plan but do not silently invent a foreground bridge owner.

## Protocol layers

The Claude harness talks to an ephemeral `127.0.0.1` HTTP listener using a
per-launch bearer. The listener admits only the supported Messages API routes,
parses bounded headers and bodies, translates Messages to Responses, and
streams translated Responses SSE back as chunked Anthropic SSE. The pipeline is
split deliberately:

1. `codex_bridge` owns loopback auth, request limits, and downstream framing.
2. `codex_translate` maps request semantics without sockets or credentials.
3. `codex_upstream` constructs the Responses request and every outbound header.
4. `codex_sse` handles fragmented upstream frames and legal downstream event
   ordering.
5. `codex_pipeline` composes those components for streaming and aggregated
   non-streaming replies.

The bridge is embedded Rust; it downloads or installs no Go, Node, proxy, or
sidecar runtime. Shipped artifacts therefore execute without a compiler or
language runtime on the target runner.

## Credentials and security boundary

Only two explicit upstream sources exist: a platform `OPENAI_API_KEY`, or a
clud-owned experimental ChatGPT subscription record created by `clud codex-auth
login --acknowledge-experimental`. If the subscription record exists, it is the
selection; an expired or invalid record asks the user to log in again and never
falls back to the platform key. `logout` deletes only
`~/.clud/codex-auth.json`, never the Codex CLI's file.

OAuth uses PKCE, random state and nonce, and a loopback callback on port 1455
(1457 only when 1455 is occupied). Unix credentials use mode 0600. Windows
applies and reads back a protected owner-only DACL before credential bytes are
written and after atomic replacement. Tokens, local bearer, account id, and
upstream URLs are excluded from debug output, plans, errors, dry runs, logs,
and subprocess diagnostics.

No downstream header is forwarded upstream and no upstream error body is
forwarded downstream. Failure diagnostics use typed, allowlisted facts; bridge
forensics are bounded and failure-only, so a healthy launch creates no file.

## Bounded failure behavior and rollback

Header/body/frame deadlines, byte caps, one active worker, finite admission
wait, and cancellation-aware socket shutdown bound hostile peers. Deterministic
SSE fragmentation/malformed-sequence tests cover parser behavior. Once any
downstream frame is visible, retry is prohibited: replay could duplicate text
or tool calls. Shutdown and drop are idempotent.

To roll back, use `--harness default` for one launch or reset the saved harness
choice in `clud settings`; neither action changes native Claude or Codex
launches. If the bridge cannot start, verify a Claude executable is on PATH,
then run `clud --codex --harness default` to use native Codex while diagnosing
the cross-route.

## Validation scope

The repository verifies protocol translation, lifecycle, secret redaction,
credential locking, and release-artifact execution through its normal target
matrix. Real platform-key and subscription smoke tests remain release-manager
operations with throwaway accounts/projects; their date, client/API versions,
and secret-free outcomes must be recorded in the release checklist before a
compatibility claim is made.

See also [launch-targets.md](launch-targets.md) for detailed resolution,
translation, retry, and storage contracts, and
[third-party-notices.md](../third-party-notices.md) for compatibility-source
attribution.
