# session_history/

Claude-harness session history for `clud -c` / `--last` (#922). For the end-to-end design, see [docs/architecture/session-history.md](../../../../docs/architecture/session-history.md).

| File | Owns |
|---|---|
| `mod.rs` | Module wiring, `new_session_uuid` (also used by the daemon's headless sessions), `rfc3339_utc` |
| `transcript.rs` | Read-only Claude JSONL view: `Transcript::active_ancestry` (follows `parentUuid`, not file order), `checkpoints`, `title`, `preview`, `content_text` (tool blocks become notes), `estimate_resume_tokens` |
| `index.rs` | Per-cwd index: `canonical_cwd`, `index_path`, `read`, `update` (fs4 lock, atomic owner-only write), `CwdIndex::upsert` (an authoritative route beats an inferred one), `Route` |
| `import.rs` | One-time legacy import: `claude_config_dir`, `project_slug`, `infer_route`, `import_once` |
| `hook.rs` | `clud session-hook` (hidden; fast path in `main.rs`): `handle` upserts an entry; `settings_fragment` builds the hook registration; recovery files become `additionalContext` |
| `recover.rs` | `ResumeMode`, the `decide` decision table, `build_recovery` (marker, summary, newest whole turns within budget), `budget_for_window` |
| `picker.rs` | `SessionPicker`, a `selector::Selector` with a sessions step and a recovery-choice step. It is listed in `selector.rs`'s terminal-ownership guard |
| `launch.rs` | `applies` / `prepare`: run from `main.rs` before target resolution. Picks a session, decides the resume, and rewrites `Args` (provider, `--resume`, `--fork-session`, or `--session-id` plus a pending recovery file) |

**Callers:**
- `main.rs` calls `launch::prepare` and dispatches `Command::SessionHook`.
- `ForegroundRuntime::apply_session_history` registers the hooks and consumes `launch::take_pending_recovery`.

**Tests:**
- Each file has sibling tests that use synthetic transcripts only. They never read real `~/.claude` history.
- `tests/test_hello.py` covers the CLI surface under `--dry-run`.
