# Gateway test modules

The production gateway remains in `../codex_bridge.rs`.
`tests/redaction_http.rs` is a child of its test module and reuses the local
fake HTTP upstream, request builder and transport assertions. It verifies
redaction on actual gateway requests without external providers or credentials.
