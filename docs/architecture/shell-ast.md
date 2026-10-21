# Shell syntax trees for the command scanner (#1962)

The PreToolUse command scanner (`clud-cmd-scan`, `block_bad_cmd*`) judges a
shell command before it runs. Until #1962 it read commands with a hand-written
splitter and tokenizer and matched rules on word positions (`words[1]`). That
failed in ways a parser does not: `git -C <dir> clone` slipped past the
extern-directory clone guard, `(cd repo && git worktree add …)` was never
captured, and of 14 ways an agent created a worktree or clone the scanner
registered 1 (#1961). Every command is now parsed by a real grammar and
evaluated without running anything, and every rule reads the result.

Code: `crates/clud-bin/src/shell_ast/` ([README](../../crates/clud-bin/src/shell_ast/README.md)).
Decision record: [DD-170](../DESIGN_DECISIONS.md#dd-170-the-command-scanner-reads-syntax-trees-and-says-what-it-cannot-know).

## Pipeline

```
command text ──► front end (per dialect) ──► walker + evaluator ──► Script
                  tree-sitter-bash            Env: variables,         invocations
                  tree-sitter-powershell      exports, cwd, pushd     constructs, loops
                  cmd.exe splitter            stack                   assigned names
                                                                      syntax_error, depth_capped
```

- **POSIX shells:** `tree-sitter-bash`.
- **PowerShell:** `tree-sitter-powershell`.
- **cmd.exe:** a small splitter (`cmd.rs`). cmd has no grammar a library can
  offer: `%VAR%` is expanded when a *line* is read, before anything on it
  runs, and `^` escapes the next character. The splitter models exactly that.

All three produce the same [`Script`]. Each [`Invocation`] is one simple
command with:

| Field | Meaning |
|---|---|
| `words` | evaluated argv; each word is `Literal(text)` or `Unknown { raw, reason }` |
| `raw` | each word as typed, quotes included (for faithful rewrites) |
| `env` | `VAR=value` prefixes for this command only |
| `redirects` | `> out`, `2>&1`, `< in`, with evaluated targets |
| `cwd` | where it runs, after every earlier `cd`/`pushd` the evaluator could follow |
| `origin` | `Script`, `Subshell`, `Substitution`, `NestedShell`, `FunctionBody` |
| `depth` | substitution/nested-shell nesting (0 = top level) |
| `pipeline` | `{id, stage, stages}` for pipelines of two or more stages |
| `loop_kind` | the innermost loop: `While`, `Until`, `ForEach`, `CStyle{infinite}`, `Do` |
| `background`, `in_error` | trailing `&`; inside a region the grammar could not parse |

`Script` adds the top-level `constructs` (substitutions, subshells, brace
groups, control flow, function definitions) for rules that must refuse what
they cannot see through, every loop kind, every statement-level assignment, and
a classified `syntax_error`.

## What evaluation resolves

Nothing here spawns a process or reads a file; a unit test enforces it.

- **Resolved:**
  - assignments, `export`, `declare -x`, `unset`
  - `$X`, `${X}`, `${X:-d}`, `${X-d}`, `${X:=d}`, `${X:+a}`
  - `~` and quoting (`'…'`, `"…"`, `$'…'`, backslashes)
  - word splitting of an unquoted expansion
  - `cd`/`pushd`/`popd` through `&&`, `;` and brace groups (a subshell or pipeline stage gets a copy)
  - `for x in a b c` over up to 16 literal items
  - the text of `bash -c`, `sh -c`, `eval`, `pwsh -Command`, `cmd /c`, and a heredoc or here-string fed to a shell. These are re-parsed in their own dialect; a child shell starts with exported variables only.
  - PowerShell `$x = …`, `$env:X`, `Set-Location`, `foreach ($b in @(…))`
- **Unknown, with a reason:**
  - `$(…)`, backticks and process substitution (`CommandSubstitution`; the commands inside are still walked)
  - `$1`, `$@`, `$?`, `$RANDOM` and `$_` (`SpecialParameter`)
  - never-assigned variables (`UnsetVariable`)
  - values that depend on a branch, a loop body or `read` (`RuntimeValue`)
  - anything not modelled, such as `${X/a/b}` or arithmetic (`Unsupported`)
- **Branches:**
  - each branch of an `if`/`case` starts from the same state;
  - afterwards, a variable or cwd that differs between paths becomes `RuntimeValue`;
  - `&&` and `||` lists are read in order, assuming each step succeeds (`cd /a || exit 1; git status` runs in `/a`).
- **Heredocs:** a heredoc body is data. An unquoted body still expands `$(…)` and backticks, so those commands are walked; the grammar parses only `$(…)` there, so the body is re-read as a double-quoted string.

### The base environment

Claude Code's Bash tool starts every call from the same base environment;
shell state does not carry from one call to the next. The scanner therefore
seeds evaluation with its own process environment (what the harness passed
the hook) and the payload's `cwd`. Hook command lines read from settings files
(`block_bad_cmd_rollout::parse_hook_command`) are parsed with an *empty* base,
so `$CLAUDE_PROJECT_DIR` reads as written.

## How rules use it

`block_bad_cmd_shell.rs` is the bridge:
- `parse_command` parses once per rule family;
- `rule_words` gives the words a rule matches. That is argv text with only the transparent `env`, `command` and `exec` launchers removed, as before #1962; unknown words read as their source text.
- `Invocation::effective_argv` peels the wider launcher set (`sudo`, `nohup`, `time`, `timeout`, `nice`, `xargs`, `stdbuf`, ...) for rules that look *through* wrappers, such as git capture.

Each rule states what it does with what cannot be known:

| Rule | Unknown program / argument | Parse error |
|---|---|---|
| `bad_commands`, `bad_pipelines` | program unknown: logged, not matched; arguments match on source text | rules run on what parsed |
| extern clone guard, worktree root guard | a clone/`worktree add` whose program or destination is unknown is **denied** (it might create an unreclaimable checkout) | as parsed |
| git clone/worktree capture (observation) | never captures an unknown destination | as parsed |
| `safe-rm` redirect | every `rm` is redirected regardless | as parsed |
| `bash.block_cd` | unknown target is `Unresolvable`: strict denies, relaxed allows | as parsed |
| hand-written wait | a `sleep` of non-literal length is refused | as parsed |
| command gate, `/grind` caps | unknown program is not the prefix: denied | **denied** (fails closed) |
| `safe_rm.extra_roots` write check | unknown writes count as writes | **denied** |
| checkout claims | unknown cwd or `GIT_DIR`: refused with "run Git in a separate tool call" | as parsed |

Pathological input fails open, as the scanner always has:
- substitutions and nested shells deeper than 8 are not walked;
- syntax nested deeper than 200 is not walked.

`Script::depth_capped` is set either way, and the hook logs it.

### Worktree location guards

Worktrees are discouraged in general (#1971); when one is needed it lives in
clud's worktree root, `~/.clud/worktrees`, where the daemon reclaims it.

- **`git worktree add`** must land under `~/.clud/worktrees` or `/grind`'s
  `<repo>/.clud/grind/worktrees/`. Anything else is refused with the
  sanctioned path, overridable for one call with
  `CLUD_BAD_CMD_OVERRIDE="git-worktree-add-outside-worktree-root:<reason>"`.
- **`.claude/worktrees/`** (`block_bad_cmd_claude_worktrees.rs`): nothing may
  create the directory or a new entry in it, in any repo or the home
  directory. That covers shell commands read from the syntax tree (`mkdir`,
  `touch`, `cp`/`mv`, `tee`, PowerShell `New-Item`/`Set-Content`/`Out-File`,
  output redirects, `git worktree add`, `git clone`) and the file tools
  (`Write`, `Edit`, `MultiEdit`, `NotebookEdit`). Work inside an existing
  checkout there stays allowed, and so does cleanup. Override id:
  `claude-worktrees-dir`.
- **Every refused `git worktree add`, under either guard, comes with the
  command to run instead** (`rewrite_worktree_add`). The rewrite keeps every
  word as typed except the destination, which becomes the first free
  `~/.clud/worktrees/<repo>-wt-<folder>`. The guard only checks which names are
  taken; it creates nothing. If the command ran after a `cd`, the rewrite
  adds `-C <dir>`. If it named no branch, git would have used one named
  after the folder, so the rewrite adds `-b <folder>`, plus the form for
  when that branch already exists. A refused agent retries once instead of
  working out the path. A symlinked `.claude/worktrees` was rejected:
  Claude Code refuses to create worktrees through one, tools that follow
  links would walk the checkout again, and cleanup would leave a dangling
  link in the repo.
- **Claude Code's own worktrees** (`--worktree`, `EnterWorktree`,
  `isolation: "worktree"` subagents) are relocated, not refused: on Claude
  Code 2.1.50+ clud registers `WorktreeCreate`/`WorktreeRemove` lines
  (`clud_hooks_compile::worktree_hooks_fragment`) whose handler
  (`claude_worktree_hook.rs`) builds `~/.clud/worktrees/<repo>-wt-<name>` on a
  `worktree-<name>` branch from `origin/<default>` (or `HEAD` under
  `worktree.baseRef: "head"`), and removes only worktrees under that root,
  never with `--force`. Older clients get no line: before 2.1.101 an
  unrecognized hook event voided the whole settings document.

## Testing

- `shell_ast/tests.rs`: the #1961 variants, the evaluation table, the Unknown policy, cwd tracking, structure, PowerShell, cmd, the latency budget (p99 < 5 ms) and the no-process rule.
- `shell_ast/fuzz_tests.rs`: seeded fuzzing over all three dialects (no panic, invariants hold), plus property tests that whitespace, quoting style, `( … )`, `{ …; }`, `env`/`command`/`exec`, comments, redirects and flag order never change the command.
- `shell_ast/oracle_tests.rs`: the **recall oracle**.
  - `oracle_fixtures/shim_oracle.json` pairs real agent Bash commands with the git/gh calls the [git/gh shim](git-gh-telemetry-shim.md) recorded them running. It is pseudonymized and regenerated with `python -m ci.refresh_shell_ast_oracle`.
  - The test reports recall and fails below the fixture's `floor`. Recall may only go up: raise the floor with any change that raises it.
- `block_bad_cmd_ast_tests.rs`: every spelling of a worktree add or clone is captured, both location guards hold however the command is spelled, and heredoc text is never captured.

## Known grammar gaps

- `tree-sitter-bash` files `cmd <<EOF | other` with the pipeline *inside* the
  heredoc redirect; the walker rebuilds the pipeline.
- `tree-sitter-powershell` reports a bare `$env:X\path` argument as an `ERROR`
  node; the command inside still parses, and its invocation is marked
  `in_error`.

[`Script`]: ../../crates/clud-bin/src/shell_ast/mod.rs
[`Invocation`]: ../../crates/clud-bin/src/shell_ast/mod.rs
