# Session history: the `clud -c` picker, index, and recovery

Issue #922. This doc covers how clud lets you continue a Claude-harness session in the current directory, whichever provider route created it (Claude, Codex via Claude, DeepSeek via Claude, …), and how it recovers one that can't be resumed natively.
Module: [`crates/clud-bin/src/session_history/`](../../crates/clud-bin/src/session_history/README.md).
Rationale: [DD-092](../DESIGN_DECISIONS.md#dd-092-clud-indexes-claude-harness-sessions-per-cwd-and-recovers-them-portably).

## Behaviour

| Invocation | What happens |
|---|---|
| `clud -c` at a terminal | Opens the picker: this directory's Claude-harness sessions, newest first. Each row shows title or prompt preview, last activity, route, estimated context, and compact checkpoints. |
| `clud --last` | Resumes the newest session in this directory without a picker. |
| `clud -c` without a terminal (scripts, pipes, `--dry-run`) | Unchanged: Claude's native `--continue`. |
| `clud -r <id>` | Unchanged: Claude resumes that exact session. |
| `clud -c` with a Codex or DeepSeek *harness* | Unchanged: that harness's own resume. Only the Claude harness is indexed. |

Selecting a session that has compact checkpoints opens a second step:
- **Full session** follows `--resume-mode` (default `auto`).
- **Compact checkpoint N** starts a portable recovery from that checkpoint.
- **Recent-history recovery** starts a portable recovery from the newest checkpoint plus recent turns.

With no provider flag, the launch uses the session's *recorded* route, so a Codex-via-Claude session goes back through Codex. An explicit flag such as `--claude`, `--codex` or `--deepseek` asks for a provider switch.

## `--resume-mode`

`recover::decide` implements this table:

| Situation | `auto` | `native` | `portable` |
|---|---|---|---|
| Same provider, within budget | native `--resume` | native | recovery |
| Explicit switch between routes that keep their whole state in Claude's transcript, within budget | `--resume --fork-session` | forked native | recovery |
| Source or destination route keeps state outside the transcript (Codex bridge, unified gateway) | recovery | **error**: use portable | recovery |
| Session larger than the budget | recovery | **error** | recovery |
| A compact checkpoint was chosen | recovery from it | **error** | recovery from it |

The **budget** is 50% of the destination model's context window, taken from `server_settings::effective_context_window` for `--model` or the session's model. When the window is unknown, clud assumes 200k. The estimate is always computed from the content a resume would replay: the active branch from its newest checkpoint onward, at a pessimistic one token per three bytes. The cumulative `usage` fields in the transcript are never summed, because they overstate the live context by orders of magnitude.

## Portable recovery

`recover::build_recovery` never slices raw JSONL. It does this:

1. It follows the **active ancestry**: the newest main-chain record, then back through `parentUuid`. File order is not conversation order once you rewind or edit a prompt.
2. It starts at the chosen checkpoint, or the newest one.
3. It emits `[CLUD RECOVERY CHECKPOINT — INCOMPLETE HISTORY]` and a sentence saying that earlier history was truncated.
4. It appends the compact summary, then the newest **whole turns** that fit, in chronological order.
   - A turn is a real user prompt plus everything up to the next one.
   - A user record that carries only tool results continues the turn it belongs to.
   - Tool calls become `[used tool X]`. Tool output is omitted, so no unmatched tool call or result is ever emitted.
5. It records lineage in the new session's index entry: `recovered_from`, the checkpoint id, and the truncated-token estimate.

The new session gets a fresh `--session-id`. The context goes into a private recovery file under `<state>/session-history/recovery/`. It is not passed on the command line, so large payloads survive Windows' argv limit. The new session's `SessionStart` hook prints the file as `hookSpecificOutput.additionalContext` and deletes it. The source transcript is never modified.

## The index

`<state>/session-history/<sha256(cwd)[..32]>.json` holds one file per canonical cwd. Symlinks are resolved, the Windows `\\?\` prefix is stripped, and case is folded on Windows. Each entry records:
- the session id and transcript path
- the **route**
- the model, title, last activity and checkpoint count
- the lineage, if the session came from a recovery

It never copies prompts or summaries. Previews come from the transcript when needed.

**Route authority.** Every Claude-harness launch registers `clud session-hook` for `SessionStart`, `PostCompact` and `SessionEnd` (`ForegroundRuntime::apply_session_history`), next to the user's hooks and the Codex bridge's own lifecycle hooks. The hook records the route clud resolved in the `LaunchPlan`, and that route is authoritative. A route guessed from a model name, during legacy import, is flagged `route_inferred` and never overwrites an authoritative one.

**Legacy import.** The first time a cwd is used, `import::import_once` reads that cwd's Claude project directory once. The directory is `$CLAUDE_CONFIG_DIR` or `~/.claude`, then `projects/<slug>`, where the slug replaces every non-alphanumeric character with `-`. Only transcripts whose recorded cwd matches exactly are imported. After that, the hooks keep the index current, and no launch rescans the tree.

**Safety.**
- Every write holds an exclusive `fs4` lock on a sibling `.lock` file and replaces the file atomically, owner-only: mode 0600 on Unix, a protected owner DACL on Windows (`fs_private`).
- A corrupt index reads as empty and is rebuilt.
- Entries whose transcript is gone are not offered.
- Nothing from the index, previews or recovery files goes to logs or `--dry-run` output. `--dry-run` never opens the picker.

## Non-goals

- Native Codex-harness history. It belongs to Codex.
- Remote sync.
- Using daemon terminal logs as conversation history.
