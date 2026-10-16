---
name: session-control
description: Use when adding or changing a clud "session control" (a chat or agent control) — a switch only the user can turn that hooks enforce on every agent in a session, subagents included, such as "worktrees allowed", set from the user's prompt or the launch environment. Covers the state route (UserPromptSubmit → daemon session-flag dictionary → PreToolUse / harness-event hooks), session identity, the CLUD_* environment passthrough, fail-closed rules, and the tests to write.
---

# Adding a session control

The facts (state route, identity, the worktree control as the worked
example) live in
[docs/architecture/session-controls.md](../../../docs/architecture/session-controls.md).
Read it first. This page is the checklist.

## Decide first

- **Who may turn it on.** Only the user: their prompt, their launch
  environment or flag, or a mode they started (an active `/grind` run).
  Never anything an agent can write: Bash text, a file in the repo, a
  `clud` subcommand, or `CLUD_BAD_CMD_OVERRIDE`. Refused agents look for an
  escape, so leave none.
- **Default and failure direction.** A control that refuses by default
  fails **closed**: no session id, daemon down, unreadable reply → refuse.
  A control that allows by default fails open. Write the choice in the
  module docs.
- **Scope.** The payload `session_id` scopes it to one conversation and
  every subagent in it; `/clear` resets it. An environment variable scopes it
  to the whole launch.

## Steps

1. **Rule module** at the crate root, like `worktree_consent.rs`: the flag
   key, the env name, a pure `decide(...)` that takes the lookup as a
   closure (so tests need no daemon), and a `refusal(session_id)` wrapper
   that reads the env and calls `crate::daemon::session_flag_get`. Register it
   in `lib.rs`.
2. **Prompt opt-in.** `UserPromptSubmit` is already registered by
   `clud_hooks_compile::deletion_safety_fragment` and routed in
   `block_bad_cmd::run_for_event`. Extend `worktree_consent::handle_prompt`,
   or call a sibling from the same branch; there is one registration for all
   controls. The handler must exit 0 and print nothing: stdout is added to the
   conversation, and exit 2 erases the prompt.
   - Match a **yes**, not a mention: split into clauses and reject a
     negation before the keyword ("no X", "don't use X", "without X").
     Keep the verb list to granting verbs; "can", "make" and "with" turn
     questions and chores into consent. Pin both directions in a unit
     table built from real prompts.
3. **Daemon state.** Use `daemon::session_flag_set` / `session_flag_get`
   with a new key. No protocol change is needed; the dictionary takes any
   JSON value. They connect only and never start a daemon.
4. **Launch opt-in.** Read `std::env::var` of a `CLUD_*` name in the hook.
   `runner::child_env` already passes `CLUD_*` from clud to `claude` to every
   hook. If a backend flag implies consent (as `--worktree` does), set the
   variable in `main.rs` next to the unsafe-mode marker, from
   `args.passthrough`.
5. **Enforcement points.** Cover every way the action happens:
   - shell commands: after the parse in `block_bad_cmd` (use
     `evaluation.git_path_captures` or the `shell_ast` invocations, never
     substring matches on the command);
   - non-shell tools: a PreToolUse check next to the worktree-consent one.
     Add each tool to `GUARDED_TOOLS` and to the predicate in
     `every_rule_tool_reaches_the_dispatcher`, or the harness never calls
     the hook;
   - harness events that perform the action (`WorktreeCreate`): refuse in
     their handler as the backstop.
6. **Refusal text** says what is off, what to do instead, and how the
   *user* turns it on. Update any other refusal that recommended the action
   now gated (the checkout-claim text did).
7. **Docs.** Add the control to `session-controls.md`, a breadcrumb in the
   touched READMEs, and a `DD-NNN` if the design is non-obvious.

## Tests

- `decide`: allowed with consent; refused without; refused when the lookup
  errors; refused with no session id; the env skips the lookup.
- The prompt table (yes and no phrasings from real transcripts).
- Each tool or event reaches the check (`every_rule_tool_reaches_the_dispatcher`,
  a fragment test in `clud_hooks_compile_tests.rs`).
- The daemon dictionary keeps sessions apart (`daemon/session_flags.rs`).

## Gotchas

- **The transcript is not the user.** Its "user" entries include task
  notifications, compaction summaries and hook text. Read the prompt from
  the `UserPromptSubmit` payload, never by scanning the transcript.
- **Subagents never see the prompt.** That is why the control lives in
  hooks: the main agent's brief to a subagent drops constraints.
- **Codex** has no prompt event mapped; there a control is env-only.
- **Unsafe mode** and `CLUD_ALLOW_ALL_CMDS` bypass PreToolUse rules; decide
  whether the harness-event backstop should honor them too.
- Validate through `bosn ci` per CLAUDE.md; for a local type check use
  `soldr --no-cache cargo check -p clud --tests`.
