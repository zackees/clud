# Reversible secret redaction

`detect.rs` owns precise credential ranges and overlap precedence.
`policy.rs` resolves flags, free-model defaults and known training policy.
`session.rs` owns the HMAC key, zeroizing mapping, JSON walk and counters;
`reversible.rs` exports that API and carries its core regression tests.
`stream.rs` restores Anthropic SSE with bounded trailing prefixes per block.
`status.rs` publishes counts when a gateway turn finishes, including failures.

The mapping has no Debug or serialization surface. Only `Counts` may enter
status state. HTTP boundary tests live in
`../codex_bridge/tests/redaction_http.rs`; final Codex send/retry/compaction
coverage lives in `../codex_upstream.rs`.
See [the architecture contract](../../../../docs/architecture/secret-redaction.md).
