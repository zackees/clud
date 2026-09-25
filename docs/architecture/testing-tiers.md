# Testing Tiers

clud is tested at three layers. Pick the lowest one that can observe what you
are asserting.

| Tier | What is real | What is faked | Answers | Where |
|---|---|---|---|---|
| **1. Fake harness** | clud | the whole harness: `mock-agent` pretends to be `claude`/`codex` | does clud launch the harness correctly (argv, env, PTY, loop markers)? | `tests/integration/`, `crates/clud-bin/tests/` |
| **2. Gateway probe** | clud + Claude Code | the model API, as a passive recorder | what does Claude Code *send* (headers, effort, model IDs)? | `tests/test_real_claude_unified_effort.py` |
| **3. Real harness, mock agent backend** | clud + Claude Code + tools, git, hooks | the model API, as a **scripted driver** | what does Claude Code *do* with clud's skills, agents, workflows and hooks? | `tests/harness/` |

Tier 3 exists because Claude Code is where everything clud installs actually
runs: skills, agent types, workflows and hooks. The only way to prove that a
flow like `/do` or `/grind` behaves correctly is to run the real harness and
fake just the model. See
[DD-090](../DESIGN_DECISIONS.md#dd-090-test-claude-code-integrations-by-mocking-the-model-not-the-harness).

## Tier 3: how it works

```
test ─ Harness ─ claude -p … (real Claude Code, isolated config)
                    │  ANTHROPIC_BASE_URL
                    ▼
            mock-agent serve --script script.json   (scripted model)
                    │  records every request
                    ▼
       logs/requests.jsonl   logs/hooks.jsonl   fake gh state   git repo
```

- **`mock-agent serve`** (`testbins/mock-agent/src/serve.rs`) answers
  `POST /v1/messages` from a JSON script. It speaks SSE and JSON and binds to
  loopback only. The script format is in
  [`testbins/mock-agent/src/README.md`](../../testbins/mock-agent/src/README.md).
- **`Harness`** (`tests/harness/harness.py`) builds the isolated world:
  - `HOME`, and `CLAUDE_CONFIG_DIR` set to `home/.claude`, populated by
    `clud install-assets --home`, so the test runs exactly the skills, agents
    and workflows users get
  - a git repo on `main` with a bare `origin`
  - a fake `gh` backed by a JSON state file
  - a hook recorder chained before the real `clud-cmd-scan`
- **Assertions** read three logs: the requests that reached the model, the
  hook calls (with each call's `agent_type`), and the world state (git refs,
  fake-GitHub PRs).

Run it with `CLUD_REAL_CLAUDE_TESTS=1 pytest tests/harness`; it needs an
installed Claude Code (`CLUD_REAL_CLAUDE` overrides the binary). In CI it runs
in full mode as `Test linux-x64 (harness)`, against the Claude Code version
pinned in `.github/workflows/_run-tests.yml`.

### Behavior the fixture relies on (Claude Code 2.1.282)

- Scripted tool calls from the backend really execute, headless via `claude -p`.
  `Agent` and `Workflow` are available.
- Subagents and workflow agents send their own system prompt and a narrower
  tool list, so the script can tell roles apart.
- A workflow agent's tool calls reach PreToolUse hooks with its `agent_type`.
- A skill's `$ARGUMENTS` and `` !`cmd` `` are rendered at invocation.
  `UserPromptSubmit` can add context but cannot rewrite a prompt.
  `disable-model-invocation` hides skills from the model, but not agent types.

### Pitfalls

1. **The turn is not "is the last message a `tool_result`".** Claude Code
   appends reminder text after tool results, and background-agent completions
   arrive as extra user messages. The backend keys each step on the number of
   assistant turns already in the request, which makes every request
   self-describing.
2. **Project `.claude/settings.json` hooks don't fire under an isolated
   `CLAUDE_CONFIG_DIR`.** The fixture passes hooks with `--settings`.
3. **Say `python`, and keep clud's session shims off PATH.** Outside a clud
   session, clud's `python` shim exits 127. The fixture strips
   `~/.clud/state/shims` and `~/.clud/state/rm-shim`, and runs its own scripts
   with the test's interpreter.
4. **`clud-cmd-scan`'s rm-identity check** requires the first `rm` on PATH to
   be byte-identical to the `clud-shim` beside the hook, or it denies every
   shell call. The fixture puts a copy of the built `clud-shim` first on PATH,
   named `rm`.
5. **Streaming is the default**, so the backend must speak SSE, including
   `input_json_delta` for tool input.

## Suites

- `tests/harness/test_smoke.py`: the framework itself (#1323).
- The `/do` suite (#1322) and the `/grind` suite (#1324) build on it.
