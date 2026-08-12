# Connector log inventory

This read-only diagnostic identifies Claude transcripts and clud bridge logs
that can be attributed to Codex or DeepSeek without printing conversation
content or raw provider error messages.

From the repository root:

```text
python -m bench.connector_logs.inventory
python -m bench.connector_logs.inventory --json
python -m bench.connector_logs.inventory --since-days 0 --show-unusable
```

Claude transcripts are usable when their structured assistant records name a
non-synthetic Codex or DeepSeek model and their `cwd` matches the requested
project. A `bridge.jsonl` file is usable for connector analysis only when its
`<pid>__<epoch>` start time correlates with one of those transcripts inside the
configured `--window-seconds` tolerance. Multiple transcript files in the same
one-second launch cluster produce provider-only attribution; unmatched files
remain explicitly unattributed.

Bridge logs can be contaminated by bridge unit tests executed inside the same
clud session. A correlated log is therefore rejected when its sibling
`reap.jsonl` records Cargo or rustc, or when the bridge records match a strong
fixture-matrix signature. This is intentionally conservative: a rejected file
may contain real events, but the current on-disk format has no per-record
provenance with which to separate them safely.

New unit and integration-test forensic logs are written to
`~/.clud/state/test-sessions/`, which this production inventory does not scan.
The contamination checks remain for logs created by older clud builds.

The report includes model IDs, timestamps, HTTP status counts, and allowlisted
error codes. It excludes prompts, responses, tool inputs/outputs, credentials,
request bodies, and raw error text.
