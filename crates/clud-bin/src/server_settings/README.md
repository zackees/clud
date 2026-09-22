# server_settings/

Server-side settings (#1192). clud bakes one JSON document into the binary,
refreshes it from `main`, and falls back section by section to the last value
that validated. The end-to-end behaviour is described in
[docs/architecture/server-settings.md](../../../../docs/architecture/server-settings.md),
and the reasoning in
[DD-072](../../../../docs/DESIGN_DECISIONS.md#dd-072-server-side-settings-are-one-baked-in-json-document-with-per-section-last-known-good).

## Files

- `mod.rs` - the public API:
  - the `Section` trait and the `SectionSpec` registry entry;
  - `Document::parse`, which checks `schema_version`;
  - `merge`, the per-section last-known-good fallback;
  - `Snapshot` and `snapshot()`, the values fixed for one process;
  - `built_in()`;
  - the `CLUD_SERVER_SETTINGS*` environment controls.
- `json.rs` - the strict JSON parser. It enforces the size cap and UTF-8,
  strips a byte-order mark, and rejects duplicate keys and trailing content.
- `store.rs` - cache reads and atomic writes, the background refresh the
  caller waits on for a bounded time, the backoff after a failure, and the
  `ureq` fetch.
- `sections.rs` - the `SECTIONS` registry and the section types:
  `DeepSeekSettings`, with `deepseek()`, `provider_default_model()`, and
  `provider_subagent_model()`; and `ModelContexts` (#1258), with
  `model_contexts()` and `effective_context_window()`, the exact context
  window for a wire ID (catalog `claude_max_context_tokens` wins, then the
  served OpenRouter datasheet row).

## Callers

- `main.rs` calls `provider_default_model` for a direct launch when neither the
  CLI nor saved settings name a model.
- `foreground_runtime.rs` calls `provider_subagent_model` and
  `effective_context_window` when it builds the child environment for an
  Anthropic-compatible provider — the latter teaching the harness an
  uncataloged wire ID's exact context window (#1258).

## Adding a section

Follow
[docs/architecture/server-settings.md#adding-a-setting](../../../../docs/architecture/server-settings.md#adding-a-setting).
