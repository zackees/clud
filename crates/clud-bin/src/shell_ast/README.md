# `shell_ast/`

Shell commands parsed into syntax trees and evaluated without running
anything (#1962). Used by the PreToolUse command scanner (`block_bad_cmd*`).
How it works and how rules use it: [shell-ast.md](../../../../docs/architecture/shell-ast.md).

| File | Contents |
|---|---|
| `mod.rs` | Public API: `parse(text, Dialect, &BaseEnv) -> Script`; `Script`, `Invocation`, `Value`/`Unknown`/`UnknownReason`, `Redirect`, `Construct`, `SyntaxError`, `LoopKind`, `Origin`, `PipelineSlot` |
| `bash.rs` | POSIX walker over `tree-sitter-bash`: commands, pipelines, lists, subshells, loops, branches, functions, heredocs and redirects, word evaluation |
| `powershell.rs` | PowerShell walker over `tree-sitter-powershell`: commands, chains, assignments, `$env:`, script blocks, loops |
| `cmd.rs` | cmd.exe splitter: `%VAR%` expanded per line, `^` escapes, `&`/`&&`/`||`/`|`, redirects, `set`, `cd /d` |
| `words.rs` | Shared walk state (`Ctx`, `Collector`), launcher unwrapping (`strip_wrappers`), nested-shell dispatch (`bash -c`, `pwsh -Command`, `cmd /c`, `eval`), syntax-error classification |
| `env.rs` | `BaseEnv` (inherited variables and cwd) and the walker's `Env`: exports, `cd`/`pushd`, branch joins |
| `paths.rs` | String-level path joins for both POSIX and Windows spellings; `program_name` |
| `tests.rs` | Behaviour tables (variants, evaluation, Unknown policy, structure, dialects, latency, no-process rule) |
| `fuzz_tests.rs` | Seeded fuzzing and spelling-equivalence properties |
| `oracle_tests.rs`, `oracle_fixtures/` | Recall against real recorded git/gh calls; refreshed by `ci/refresh_shell_ast_oracle.py` |

Callers: `block_bad_cmd_shell.rs` (`parse_command`, `rule_words`) and
`block_bad_cmd_rollout.rs` (`parse_hook_command`).
