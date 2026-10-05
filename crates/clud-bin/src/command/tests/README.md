# command/tests/

Unit tests for `command::builder`'s `LaunchPlan` construction, split out of the
parent `tests.rs` by the LOC guard. Each submodule keeps its tests verbatim and
pulls the shared helpers (`parse`, `plan`, `plan_at`, `last_arg`,
`command_without_deletion_policy`, the `*_target()` fixtures,
`console_launch_mode`) in through `use super::*;`.

- `builtin_prompts.rs` — the builtin-command prompt builders: `up`,
  `rebase`, `fix`, `do` (including the meta-issue `/grind` redirect), and the
  `/grind` seed prompt.
- `codex_native_routes.rs` — Codex-native argv assembly: the `exec` /
  `resume` subcommand, `--profile`-vs-prompt positional handling, native
  agents, `CODEX.md` project-doc fallback, and `--resume` built-in seeding.
- `grind_launch.rs` — the `/grind` launch contract: one interactive session
  with harness-owned repetition, the Claude-harness requirement, and the
  `grind reconcile` exemption (#1803).
- `loop_contract.rs` — `clud loop` iteration/marker behavior and Claude
  `stream-json` progress injection.
- `repeat_parse.rs`, `repeat_schedule.rs`, `repeat_execution.rs` — the
  `--repeat` parser, the no-overlap scheduler, and repeat execution.

The `LaunchPlan` contract itself is documented at
[docs/architecture/launch-plan.md](../../../../../../docs/architecture/launch-plan.md).