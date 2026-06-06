//! Issue #262: `clud memory *` CLI verb dispatch.
//!
//! All mutating verbs proxy through the daemon's `/memory/*` HTTP routes
//! (single SQLite writer per process — DD-018). Read-only verbs that
//! only inspect the daemon's view of the store (`status`, `search`) also
//! use HTTP. The two verbs that touch the local working tree directly
//! (`branch-isolate` / `branch-unisolate`) call the library functions
//! `memory::identity::branch_isolate` / `branch_unisolate` against the
//! current repo's `git common-dir`.
//!
//! Exit codes:
//! - `0` success.
//! - `1` user error (validation, unknown id, missing query).
//! - `2` internal error (HTTP 5xx, JSON decode failure, IO error).
//! - `3` daemon unavailable / `--no-daemon`.

use std::io::{self, BufRead, Write};
use std::path::Path;

use clap::CommandFactory;
use serde_json::json;

use crate::args::{Args, MemorySubcommand};
use crate::daemon::{self, MemoryHttpResponse};
use crate::memory::identity;

/// Dispatch a `clud memory <sub>` invocation. Returns the process exit
/// code.
pub fn run(args: &Args, sub: Option<MemorySubcommand>) -> i32 {
    // Bare `clud memory` prints help.
    let Some(sub) = sub else {
        return print_help_and_exit_zero();
    };

    // Verbs that touch local working-tree state and never need the
    // daemon. `--to-disk` / `--from-disk` are #264 stubs and exit
    // cleanly without contacting the daemon.
    match &sub {
        MemorySubcommand::BranchIsolate => return cmd_branch_isolate(),
        MemorySubcommand::BranchUnisolate => return cmd_branch_unisolate(),
        MemorySubcommand::Export { to_disk, .. } if *to_disk => return cmd_export_to_disk_stub(),
        MemorySubcommand::Import { from_disk, .. } if *from_disk => {
            return cmd_import_from_disk_stub();
        }
        _ => {}
    }

    // The rest require the daemon.
    if args.no_daemon {
        eprintln!("error: memory operations require the clud daemon; remove --no-daemon");
        return 3;
    }
    let state_dir = match daemon::default_state_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve clud state dir: {e}");
            return 2;
        }
    };
    if let Err(e) = daemon::ensure_daemon(&state_dir) {
        eprintln!("error: daemon unavailable: {e}");
        return 3;
    }

    match sub {
        MemorySubcommand::Init => cmd_init(&state_dir),
        MemorySubcommand::Status { json } => cmd_status(&state_dir, json),
        MemorySubcommand::Search {
            query,
            k,
            session_id,
            tier_floor,
            scope_key,
            json,
        } => cmd_search(
            &state_dir,
            &query,
            k,
            session_id.as_deref(),
            tier_floor.as_deref(),
            scope_key.as_deref(),
            json,
        ),
        MemorySubcommand::Save {
            content,
            tier,
            session_id,
            metadata,
            json,
        } => cmd_save(
            &state_dir,
            &content,
            &tier,
            session_id.as_deref(),
            metadata.as_deref(),
            json,
        ),
        MemorySubcommand::Forget { id, json } => cmd_forget(&state_dir, &id, json),
        MemorySubcommand::Export { .. } => cmd_export_to_stdout(&state_dir),
        MemorySubcommand::Import { from_stdin, .. } if from_stdin => {
            cmd_import_from_stdin(&state_dir)
        }
        MemorySubcommand::Import { .. } => cmd_import_default(),
        MemorySubcommand::Ui { no_open } => cmd_ui(&state_dir, no_open),
        MemorySubcommand::Reembed { model, dry_run } => {
            cmd_reembed(&state_dir, model.as_deref(), dry_run)
        }
        MemorySubcommand::BranchIsolate | MemorySubcommand::BranchUnisolate => {
            // Already handled above; unreachable.
            0
        }
    }
}

fn print_help_and_exit_zero() -> i32 {
    let mut top = Args::command();
    match top.find_subcommand_mut("memory") {
        Some(mem) => {
            let _ = mem.print_help();
            println!();
            0
        }
        None => {
            eprintln!("error: memory subcommand definition missing (internal bug)");
            2
        }
    }
}

// ---------- handlers ----------

fn cmd_init(state_dir: &Path) -> i32 {
    // `ensure_daemon` already ran. Touch the stats endpoint to confirm
    // the memory service is alive and surface the resolved paths +
    // embedder name.
    match daemon::http_stats(state_dir) {
        Ok(resp) if resp.status == 200 => {
            let memory_dir = state_dir.join("memory");
            println!("memory: initialized");
            println!("  state dir       : {}", state_dir.display());
            println!(
                "  db path         : {}",
                memory_dir.join("memory.db").display()
            );
            println!(
                "  tantivy dir     : {}",
                memory_dir.join("tantivy").display()
            );
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&resp.body) {
                if let Some(name) = parsed.get("embedder_status").and_then(|v| v.as_str()) {
                    println!("  embedder        : {name}");
                }
                if let Some(dim) = parsed.get("embedder_dim").and_then(|v| v.as_u64()) {
                    println!("  embedder dim    : {dim}");
                }
                if let Some(dim) = parsed.get("store_embed_dim").and_then(|v| v.as_u64()) {
                    println!("  store embed dim : {dim}");
                }
                if let Some(uv) = parsed.get("schema_user_version").and_then(|v| v.as_u64()) {
                    println!("  schema version  : {uv}");
                }
            }
            0
        }
        Ok(resp) if resp.status == 503 => {
            eprintln!("error: memory subsystem unavailable on running daemon");
            eprintln!("note: try `clud daemon restart` to retry the embedder load");
            2
        }
        Ok(resp) => {
            eprintln!("error: init failed ({}): {}", resp.status, resp.body);
            2
        }
        Err(e) => {
            eprintln!("error: init failed: {e}");
            2
        }
    }
}

fn cmd_status(state_dir: &Path, want_json: bool) -> i32 {
    match daemon::http_stats(state_dir) {
        Ok(resp) if resp.status == 200 => {
            if want_json {
                println!("{}", resp.body);
                return 0;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
            let counts = parsed.get("tier_counts").cloned().unwrap_or_default();
            let working = counts.get("working").and_then(|v| v.as_u64()).unwrap_or(0);
            let episodic = counts.get("episodic").and_then(|v| v.as_u64()).unwrap_or(0);
            let semantic = counts.get("semantic").and_then(|v| v.as_u64()).unwrap_or(0);
            let embedder = parsed
                .get("embedder_status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let edim = parsed
                .get("embedder_dim")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let sdim = parsed
                .get("store_embed_dim")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let uv = parsed
                .get("schema_user_version")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let interval = parsed
                .get("consolidate_interval_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = working + episodic + semantic;
            println!("clud memory  ({total} rows)");
            println!(
                "  db path        : {}",
                state_dir.join("memory").join("memory.db").display()
            );
            println!("  embedder       : {embedder} (dim={edim}, store_dim={sdim})");
            println!("  schema version : {uv}");
            println!(
                "  by tier        : working {working}  episodic {episodic}  semantic {semantic}"
            );
            println!("  consolidate    : every {interval} ms");
            0
        }
        Ok(resp) if resp.status == 503 => {
            eprintln!("error: memory subsystem unavailable");
            2
        }
        Ok(resp) => {
            eprintln!("error: status failed ({}): {}", resp.status, resp.body);
            2
        }
        Err(e) => {
            eprintln!("error: status request failed: {e}");
            2
        }
    }
}

fn cmd_search(
    state_dir: &Path,
    query: &str,
    k: u32,
    session_id: Option<&str>,
    tier_floor: Option<&str>,
    scope_key: Option<&str>,
    want_json: bool,
) -> i32 {
    if query.is_empty() {
        eprintln!("error: query must not be empty");
        return 1;
    }
    match daemon::http_search(state_dir, query, k, session_id, tier_floor, scope_key) {
        Ok(resp) if resp.status == 200 => {
            if want_json {
                println!("{}", resp.body);
                return 0;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
            let empty = Vec::new();
            let hits = parsed.as_array().unwrap_or(&empty);
            if hits.is_empty() {
                println!("(no matches)");
                return 0;
            }
            for hit in hits {
                let id = hit.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let tier = hit.get("tier").and_then(|v| v.as_str()).unwrap_or("?");
                let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let content = hit.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let trimmed = excerpt(content, 80);
                println!("{id}  [{tier}]  score={score:.4}  {trimmed}");
            }
            0
        }
        Ok(resp) if resp.status == 400 => {
            eprintln!("error: invalid search request: {}", resp.body);
            1
        }
        Ok(resp) if resp.status == 503 => {
            eprintln!("error: memory subsystem unavailable");
            2
        }
        Ok(resp) => {
            eprintln!("error: search failed ({}): {}", resp.status, resp.body);
            2
        }
        Err(e) => {
            eprintln!("error: search request failed: {e}");
            2
        }
    }
}

fn cmd_save(
    state_dir: &Path,
    content: &str,
    tier: &str,
    session_id: Option<&str>,
    metadata: Option<&str>,
    want_json: bool,
) -> i32 {
    if content.is_empty() {
        eprintln!("error: content must not be empty");
        return 1;
    }
    let payload = json!({
        "content": content,
        "tier": tier,
        "session_id": session_id,
        "metadata_json": metadata,
    })
    .to_string();
    match daemon::http_save(state_dir, &payload) {
        Ok(resp) if resp.status == 200 => {
            if want_json {
                println!("{}", resp.body);
                return 0;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
            let id = parsed.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let saved_tier = parsed.get("tier").and_then(|v| v.as_str()).unwrap_or(tier);
            println!("saved id={id}  tier={saved_tier}");
            0
        }
        Ok(resp) if resp.status == 400 => {
            eprintln!("error: invalid save request: {}", resp.body);
            1
        }
        Ok(resp) if resp.status == 503 => {
            eprintln!("error: memory subsystem unavailable");
            2
        }
        Ok(resp) => {
            eprintln!("error: save failed ({}): {}", resp.status, resp.body);
            2
        }
        Err(e) => {
            eprintln!("error: save request failed: {e}");
            2
        }
    }
}

fn cmd_forget(state_dir: &Path, id: &str, want_json: bool) -> i32 {
    if id.is_empty() {
        eprintln!("error: id must not be empty");
        return 1;
    }
    match daemon::http_forget(state_dir, id) {
        Ok(resp) if resp.status == 200 => {
            if want_json {
                println!("{}", resp.body);
                return 0;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
            let forgotten = parsed
                .get("forgotten")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if forgotten {
                println!("forgot id={id}");
            } else {
                println!("(no row with id={id})");
            }
            0
        }
        Ok(resp) if resp.status == 400 => {
            eprintln!("error: invalid forget request: {}", resp.body);
            1
        }
        Ok(resp) if resp.status == 503 => {
            eprintln!("error: memory subsystem unavailable");
            2
        }
        Ok(resp) => {
            eprintln!("error: forget failed ({}): {}", resp.status, resp.body);
            2
        }
        Err(e) => {
            eprintln!("error: forget request failed: {e}");
            2
        }
    }
}

fn cmd_export_to_stdout(state_dir: &Path) -> i32 {
    // Dump the recent list as JSON-lines. The daemon's `/memory/recent`
    // route is the seam; we cap at a generous limit so a daemon with N
    // rows can be exported in one pull.
    match daemon::http_recent(state_dir, 100_000) {
        Ok(resp) if resp.status == 200 => {
            let parsed: serde_json::Value =
                serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
            let empty = Vec::new();
            let rows = parsed.as_array().unwrap_or(&empty);
            let mut out = io::stdout().lock();
            for row in rows {
                let line = serde_json::to_string(row).unwrap_or_default();
                if writeln!(out, "{line}").is_err() {
                    return 2;
                }
            }
            0
        }
        Ok(resp) => {
            eprintln!("error: export failed ({}): {}", resp.status, resp.body);
            2
        }
        Err(e) => {
            eprintln!("error: export request failed: {e}");
            2
        }
    }
}

fn cmd_export_to_disk_stub() -> i32 {
    println!("memory export --to-disk: deferred; see #264 for git-artifact serialization");
    0
}

fn cmd_import_default() -> i32 {
    eprintln!("error: pass --from-stdin to import JSON-lines, or --from-disk (see #264)");
    1
}

fn cmd_import_from_stdin(state_dir: &Path) -> i32 {
    let stdin = io::stdin();
    let mut imported = 0usize;
    let mut errored = 0usize;
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            errored += 1;
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("warn: skipping malformed JSON line: {err}");
                errored += 1;
                continue;
            }
        };
        let content = parsed.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() {
            eprintln!("warn: skipping row with empty content");
            errored += 1;
            continue;
        }
        let tier = parsed
            .get("tier")
            .and_then(|v| v.as_str())
            .unwrap_or("working");
        let session_id = parsed.get("session_id").and_then(|v| v.as_str());
        let payload = json!({
            "content": content,
            "tier": tier,
            "session_id": session_id,
        })
        .to_string();
        match daemon::http_save(state_dir, &payload) {
            Ok(resp) if resp.status == 200 => imported += 1,
            Ok(resp) => {
                eprintln!("warn: save failed ({}): {}", resp.status, resp.body);
                errored += 1;
            }
            Err(e) => {
                eprintln!("warn: save request failed: {e}");
                errored += 1;
            }
        }
    }
    println!("imported: {imported}  skipped: {errored}");
    if errored > 0 && imported == 0 {
        2
    } else {
        0
    }
}

fn cmd_import_from_disk_stub() -> i32 {
    println!("memory import --from-disk: deferred; see #264 for git-artifact serialization");
    0
}

fn cmd_ui(state_dir: &Path, no_open: bool) -> i32 {
    let info = match daemon::read_dashboard_info(state_dir) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("error: cannot read daemon info: {e}");
            return 2;
        }
    };
    let Some(port) = info.dashboard_port else {
        eprintln!(
            "error: daemon (pid {}) has no dashboard listener; restart it via `clud daemon restart`",
            info.pid
        );
        return 2;
    };
    let url = format!("{}#memory", daemon::dashboard_url_from_info(port));
    println!("{url}");
    if no_open {
        return 0;
    }
    if let Err(e) = open::that_detached(&url) {
        eprintln!("note: could not auto-open browser ({e}); paste the URL above");
        return 1;
    }
    0
}

fn cmd_reembed(state_dir: &Path, model: Option<&str>, dry_run: bool) -> i32 {
    if let Some(m) = model {
        eprintln!("note: --model {m} is not honored in v1; daemon uses CLUD_MEMORY_EMBEDDER_*");
    }
    // The cheap path: count rows via /memory/stats, then either report
    // (dry-run) or call reembed_all in-process against the daemon's
    // SQLite handle. Because the daemon owns the writer, a true reembed
    // would need a `POST /memory/reembed` route. For v1 we offer
    // dry-run-only against the running daemon and call out the
    // requirement to stop the daemon before a real reembed.
    match daemon::http_stats(state_dir) {
        Ok(resp) if resp.status == 200 => {
            let parsed: serde_json::Value =
                serde_json::from_str(&resp.body).unwrap_or(serde_json::Value::Null);
            let counts = parsed.get("tier_counts").cloned().unwrap_or_default();
            let working = counts.get("working").and_then(|v| v.as_u64()).unwrap_or(0);
            let episodic = counts.get("episodic").and_then(|v| v.as_u64()).unwrap_or(0);
            let semantic = counts.get("semantic").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = working + episodic + semantic;
            println!("reembed: {total} rows would be re-embedded ({working} working, {episodic} episodic, {semantic} semantic)");
            if dry_run {
                return 0;
            }
            eprintln!("note: live reembed requires a dedicated daemon route; for now stop the daemon and rerun with --dry-run to plan");
            0
        }
        Ok(resp) => {
            eprintln!("error: reembed failed ({}): {}", resp.status, resp.body);
            2
        }
        Err(e) => {
            eprintln!("error: reembed request failed: {e}");
            2
        }
    }
}

fn cmd_branch_isolate() -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve cwd: {e}");
            return 2;
        }
    };
    let scope = match identity::resolve_repo_scope(&cwd) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: not a git repo: {e}");
            return 1;
        }
    };
    if let Err(e) = identity::branch_isolate(&scope.common_dir) {
        eprintln!("error: branch-isolate failed: {e}");
        return 2;
    }
    let updated = match identity::resolve_repo_scope(&cwd) {
        Ok(s) => s,
        Err(_) => scope,
    };
    println!("branch isolated.");
    println!("  scope_key : {}", updated.key);
    println!(
        "  marker    : {}",
        updated
            .common_dir
            .join(identity::BRANCH_ISOLATE_MARKER)
            .display()
    );
    0
}

fn cmd_branch_unisolate() -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve cwd: {e}");
            return 2;
        }
    };
    let scope = match identity::resolve_repo_scope(&cwd) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: not a git repo: {e}");
            return 1;
        }
    };
    if let Err(e) = identity::branch_unisolate(&scope.common_dir) {
        eprintln!("error: branch-unisolate failed: {e}");
        return 2;
    }
    let updated = identity::resolve_repo_scope(&cwd).unwrap_or(scope);
    println!("branch un-isolated.");
    println!("  scope_key : {}", updated.key);
    0
}

fn excerpt(s: &str, max: usize) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

// Re-export so the `print_help_and_exit_zero` call site can use it
// through the existing `Args::command()` factory; the trait import lives
// here intentionally so consumers of `memory::cli` get the full surface.
#[allow(dead_code)]
fn _ensure_response_used(r: &MemoryHttpResponse) -> u16 {
    r.status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_collapses_whitespace_and_truncates() {
        let long = "a".repeat(200);
        let trimmed = excerpt(&long, 80);
        // One leading ellipsis byte is added when truncation happens.
        assert!(trimmed.chars().count() <= 80);
    }

    #[test]
    fn excerpt_passes_short_input_unchanged() {
        assert_eq!(excerpt("hello world", 80), "hello world");
    }

    #[test]
    fn excerpt_collapses_internal_whitespace() {
        assert_eq!(excerpt("hello\n\tworld", 80), "hello world");
    }
}
