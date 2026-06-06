# mock-hooks-payloads/

Canned JSON fixtures for the four `clud hook` subcommands. Tests pipe
these into `clud hook <verb>` over stdin to exercise the
session-lifecycle plumbing without needing a real Claude Code or Codex
session.

The shapes mirror the live payload contracts decoded by
[`crates/clud-bin/src/hooks.rs`](../../crates/clud-bin/src/hooks.rs):
`SessionStartPayload`, `UserPromptSubmitPayload`, `PostToolUsePayload`,
and `StopPayload`. Every field is `#[serde(default)]` on the Rust side,
so partial fixtures still deserialize.

## Catalog

| File                                            | Hook verb              | Variant                                          |
|-------------------------------------------------|------------------------|--------------------------------------------------|
| `session_start_claude.json`                     | `session-start`        | Claude Code shape (`session_id`, `cwd`)          |
| `session_start_codex.json`                      | `session-start`        | Codex shape (`session-id`, `working-directory`)  |
| `user_prompt_submit_no_directive.json`          | `user-prompt-submit`   | No `remember:` / `save this:` directive          |
| `user_prompt_submit_with_directive.json`        | `user-prompt-submit`   | Has a `remember:` directive (triggers save)      |
| `user_prompt_submit_codex.json`                 | `user-prompt-submit`   | Codex variant with `save this:` directive        |
| `post_tool_use_bash.json`                       | `post-tool-use`        | Claude Bash tool call + response                 |
| `post_tool_use_codex.json`                      | `post-tool-use`        | Codex variant using `tool_call`/`tool_result`    |
| `stop_claude.json`                              | `stop`                 | Claude `Stop` with `reason: user_quit`           |
| `stop_codex.json`                               | `stop`                 | Codex `session_end` with `reason: task_done`     |

## How to use from Python

```python
from pathlib import Path
fixture = Path("testbins/mock-hooks-payloads/fixtures/session_start_claude.json")
payload = fixture.read_text(encoding="utf-8")
subprocess.run(["clud", "hook", "session-start"], input=payload, text=True, check=True)
```

## How to use from Rust

```rust
let fixture = std::fs::read_to_string(
    "testbins/mock-hooks-payloads/fixtures/session_start_claude.json"
)?;
```

## Refresh cadence

The catalog should be re-captured against the live Claude Code and Codex
hook contracts quarterly. Cross-issue tracking for the cadence lives in
the comment thread on issue #266.

## Why a sibling testbin and not a `mock-agent` extension?

`mock-agent` is the canonical PTY-target fake for the agent itself. The
hook subcommands are short-lived `clud hook <verb>` subprocesses driven
by Claude/Codex; the fake here is the **payload**, not the agent. Two
concerns, two locations.
