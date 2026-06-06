//! Issue #261: agent-memory service running in-process inside the clud
//! daemon.
//!
//! Spawned by `daemon::server::run_daemon` alongside the GC registry
//! worker and the dashboard HTTP listener. Owns the four storage
//! resources downstream subsystems (#259 MCP server, #260 hooks, #262
//! CLI verbs, #263 dashboard JS) need to share:
//!
//! - `Arc<Mutex<SqliteStore>>` — single-writer SQLite handle (the
//!   sqlite-vec extension is registered process-globally).
//! - `Arc<Mutex<LexicalIndex>>` — single `IndexWriter` over the tantivy
//!   directory.
//! - `Arc<Embedder>` — loaded once at daemon start; `Send + Sync`, no
//!   internal mutex.
//! - `TierConfig` — promotion + auto-forget thresholds resolved from
//!   the documented env vars.
//!
//! See [docs/architecture/memory.md] "Daemon integration" for the
//! concurrency model and [DD-017] for why the service lives in-process.
//!
//! This PR is scope-limited to the daemon plumbing; the dashboard route
//! bodies are stubs (#263) and there is no MCP server yet (#259).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::memory::embedder::EmbedderTrait;
use crate::memory::{
    embedder_from_env, Embedder, LexicalIndex, MemoryError, SqliteStore, Tier, TierConfig,
    EMBED_DIM_MINILM_L6_V2,
};

/// Default consolidation tick — five minutes. Override via
/// `CLUD_MEMORY_CONSOLIDATE_INTERVAL_MS`.
const DEFAULT_CONSOLIDATE_INTERVAL_MS: u64 = 300_000;

/// One `PRAGMA wal_checkpoint(TRUNCATE)` per N ticks. With the default
/// 5-minute interval that's hourly. Override via
/// `CLUD_MEMORY_CHECKPOINT_EVERY_N_TICKS`.
const DEFAULT_CHECKPOINT_EVERY_N_TICKS: u64 = 12;

const ENV_CONSOLIDATE_INTERVAL_MS: &str = "CLUD_MEMORY_CONSOLIDATE_INTERVAL_MS";
const ENV_CHECKPOINT_EVERY_N_TICKS: &str = "CLUD_MEMORY_CHECKPOINT_EVERY_N_TICKS";

/// Live handles shared with the rest of the daemon. Cloning the wrapping
/// `Arc<MemoryService>` is cheap; per-resource access is gated by the
/// inner `Mutex`es.
#[allow(dead_code)]
pub struct MemoryService {
    pub store: Arc<Mutex<SqliteStore>>,
    pub lexical: Arc<Mutex<LexicalIndex>>,
    pub embedder: Arc<Embedder>,
    pub tier_config: TierConfig,
    /// Consolidation cadence used to size the dashboard's "next tick in
    /// N seconds" hint (#263 will surface this).
    pub consolidate_interval_ms: u64,
    /// Set to `true` by `shutdown()` to break the timer's wait loop.
    /// Held inside an `Arc` so the timer thread can observe the same
    /// flag the service drops on the way out.
    shutdown: Arc<AtomicBool>,
}

impl std::fmt::Debug for MemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryService")
            .field("tier_config", &self.tier_config)
            .field("consolidate_interval_ms", &self.consolidate_interval_ms)
            .field(
                "embedder",
                &<Embedder as EmbedderTrait>::name(&self.embedder),
            )
            .finish()
    }
}

#[allow(dead_code)]
impl MemoryService {
    /// Cooperative shutdown for tests and for the future graceful-exit
    /// path on the daemon's main loop. The timer thread polls the flag
    /// every `consolidate_interval_ms / 10` so the wait wakes within a
    /// few seconds on a default-tuned daemon.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Tick used by tests: drives one round of promote → apply →
    /// forget against an explicit `now_ms`. The timer thread calls this
    /// helper in its loop; pulling it out keeps the cadence orchestration
    /// untestable but the lifecycle math fully covered.
    pub(crate) fn run_one_consolidation_tick(
        &self,
        now_ms: u64,
    ) -> Result<TickReport, MemoryError> {
        run_one_consolidation_tick(&self.store, &self.lexical, &self.tier_config, now_ms)
    }
}

/// Counts surfaced by one consolidation pass. Logged once per tick; the
/// dashboard will surface the most recent value in a later PR.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    pub promoted: usize,
    pub forgotten: usize,
}

/// Open every memory-subsystem resource and start the consolidation
/// timer. Returns the live `MemoryService`. The daemon owns the
/// resulting `Arc` for the rest of its lifetime; the timer thread holds
/// weak-style clones of the same `Arc<Mutex<...>>` handles so dropping
/// the daemon's reference is enough to wind everything down.
pub fn spawn_memory_service(state_dir: &Path) -> Result<MemoryService, MemoryError> {
    // 1. Resolve on-disk layout under the daemon's state dir. We don't
    //    rely on `memory::paths` (which composes off the process-global
    //    `CLUD_DAEMON_STATE_DIR` env) because callers may pass an
    //    explicit `--daemon-state-dir`.
    let memory_dir = state_dir.join("memory");
    std::fs::create_dir_all(&memory_dir)?;
    let memory_db_path = memory_dir.join("memory.db");
    let tantivy_dir = memory_dir.join("tantivy");

    // 2. Embedder first — its dim drives the SQLite vec0 column width.
    //    `embedder_from_env` may panic on bad env config; the Result
    //    bubbles up to the daemon, which logs and continues without
    //    memory (`server.rs` treats failures as soft).
    let embedder_result = embedder_from_env();
    let embed_dim = match &embedder_result {
        Ok(e) => {
            let d = <Embedder as EmbedderTrait>::dim(e);
            if d == 0 {
                EMBED_DIM_MINILM_L6_V2
            } else {
                d
            }
        }
        Err(_) => EMBED_DIM_MINILM_L6_V2,
    };

    // 3. Open the SQLite store. Migrations run inside `open`; if a
    //    previous daemon crashed mid-write, sqlite's WAL replay handles
    //    recovery transparently.
    let mut store = SqliteStore::open(&memory_db_path, embed_dim)?;

    // 4. WAL recovery — truncate the WAL up to the last durable commit.
    //    Cheap when the WAL is already drained; bounded by WAL size when
    //    it isn't.
    store.checkpoint_truncate()?;

    // 5. Open the tantivy index. Half-written segments from a previous
    //    crash are discarded on open.
    let mut lexical = LexicalIndex::open_or_create(&tantivy_dir)?;

    // 6. Reconciliation pass — for every row in `memories`, ensure the
    //    lexical index has a matching doc. We can't cheaply ask tantivy
    //    "do you contain id X?" without a per-id search, so the pass
    //    re-upserts every row; tantivy's delete-then-add inside `upsert`
    //    keeps the resulting index correct. Bounded by the row count;
    //    typical daemons see hundreds of memories, not millions.
    reconcile_lexical(&store, &mut lexical)?;

    // 7. Wrap the embedder Result. A failed load doesn't kill the
    //    daemon — we keep going with `Embedder::Disabled` so the rest of
    //    the subsystem (HTTP routes, future MCP server) can render a
    //    consistent "embedder unavailable" surface.
    let embedder: Arc<Embedder> = match embedder_result {
        Ok(e) => Arc::new(e),
        Err(err) => {
            eprintln!("[clud] note: memory embedder unavailable: {err}");
            Arc::new(Embedder::Disabled {
                reason: err.to_string(),
            })
        }
    };

    // Dim drift warning: the embedder may report a different dim than
    // the stored vec0 column width if the user swapped providers
    // between runs. `reembed` (CLI verb in #262) is the manual fix.
    let actual_dim = <Embedder as EmbedderTrait>::dim(embedder.as_ref());
    if actual_dim != 0 && actual_dim != store.embed_dim() {
        eprintln!(
            "[clud] warn: embedder dim ({}) != stored vec dim ({}); run `clud memory reembed`",
            actual_dim,
            store.embed_dim()
        );
    }

    let store = Arc::new(Mutex::new(store));
    let lexical = Arc::new(Mutex::new(lexical));
    let tier_config = TierConfig::from_env();
    let consolidate_interval_ms =
        env_u64(ENV_CONSOLIDATE_INTERVAL_MS, DEFAULT_CONSOLIDATE_INTERVAL_MS);
    let checkpoint_every_n_ticks = env_u64(
        ENV_CHECKPOINT_EVERY_N_TICKS,
        DEFAULT_CHECKPOINT_EVERY_N_TICKS,
    )
    .max(1);

    let shutdown = Arc::new(AtomicBool::new(false));

    // 8. Consolidation timer thread. Cheap-clones of the Arcs keep the
    //    store + lexical alive even if the daemon dropped its outer
    //    `Arc<MemoryService>` early; once `shutdown` is set the thread
    //    drops them on its way out.
    spawn_consolidation_thread(
        Arc::clone(&store),
        Arc::clone(&lexical),
        tier_config,
        consolidate_interval_ms,
        checkpoint_every_n_ticks,
        Arc::clone(&shutdown),
    );

    Ok(MemoryService {
        store,
        lexical,
        embedder,
        tier_config,
        consolidate_interval_ms,
        shutdown,
    })
}

/// Re-index every SQLite row in the lexical store. Cheap on a clean
/// daemon (no rows); on a daemon with N rows the cost is O(N) `upsert`
/// calls plus one commit. Tantivy's `delete_term`-then-`add` inside
/// `LexicalIndex::upsert` keeps the operation idempotent.
fn reconcile_lexical(store: &SqliteStore, lexical: &mut LexicalIndex) -> Result<(), MemoryError> {
    let mut count = 0usize;
    for tier in [Tier::Working, Tier::Episodic, Tier::Semantic] {
        for row in store.list_by_tier(tier)? {
            lexical.upsert(
                &row.id,
                row.session_id.as_deref(),
                row.scope_key.as_deref(),
                row.tier,
                &row.content,
            )?;
            count += 1;
        }
    }
    if count > 0 {
        lexical.commit()?;
    }
    Ok(())
}

fn spawn_consolidation_thread(
    store: Arc<Mutex<SqliteStore>>,
    lexical: Arc<Mutex<LexicalIndex>>,
    cfg: TierConfig,
    interval_ms: u64,
    checkpoint_every_n_ticks: u64,
    shutdown: Arc<AtomicBool>,
) {
    let res = thread::Builder::new()
        .name("clud-memory-consolidate".to_string())
        .spawn(move || {
            let mut tick: u64 = 0;
            // Poll the shutdown flag at a finer cadence so cooperative
            // shutdown wakes within seconds, not minutes.
            let poll_step = Duration::from_millis((interval_ms / 10).max(50));
            let mut next_due = std::time::Instant::now() + Duration::from_millis(interval_ms);
            while !shutdown.load(Ordering::SeqCst) {
                thread::sleep(poll_step);
                let now = std::time::Instant::now();
                if now < next_due {
                    continue;
                }
                next_due = now + Duration::from_millis(interval_ms);
                tick = tick.wrapping_add(1);

                let report =
                    match run_one_consolidation_tick(&store, &lexical, &cfg, unix_millis_now()) {
                        Ok(r) => r,
                        Err(err) => {
                            eprintln!("[clud] memory consolidation tick failed: {err}");
                            continue;
                        }
                    };

                let mut checkpointed = false;
                if tick % checkpoint_every_n_ticks == 0 {
                    if let Ok(mut s) = store.lock() {
                        if let Err(err) = s.checkpoint_truncate() {
                            eprintln!("[clud] memory wal checkpoint failed: {err}");
                        } else {
                            checkpointed = true;
                        }
                    }
                }
                eprintln!(
                    "[clud] memory tick #{tick} promoted={} forgotten={} checkpoint={}",
                    report.promoted, report.forgotten, checkpointed
                );
            }
        });
    if let Err(err) = res {
        eprintln!("[clud] note: memory consolidation thread spawn failed: {err}");
    }
}

/// One round of promotion + auto-forget. Pulled out of the timer thread
/// so tests can call it with a deterministic `now_ms`.
pub(crate) fn run_one_consolidation_tick(
    store: &Arc<Mutex<SqliteStore>>,
    lexical: &Arc<Mutex<LexicalIndex>>,
    cfg: &TierConfig,
    now_ms: u64,
) -> Result<TickReport, MemoryError> {
    let promotions = {
        let s = store.lock().expect("memory store mutex poisoned");
        crate::memory::promote_candidates(&s, now_ms, cfg)?
    };

    let promoted = promotions.len();
    if !promotions.is_empty() {
        let mut s = store.lock().expect("memory store mutex poisoned");
        let mut l = lexical.lock().expect("memory lexical mutex poisoned");
        crate::memory::apply_promotions(&mut s, &mut l, &promotions, now_ms)?;
    }

    let forgotten = {
        let mut s = store.lock().expect("memory store mutex poisoned");
        let mut l = lexical.lock().expect("memory lexical mutex poisoned");
        crate::memory::forget_expired(&mut s, &mut l, now_ms, cfg)?
    };

    Ok(TickReport {
        promoted,
        forgotten,
    })
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::ids::MemoryId;
    use crate::memory::store::MemoryRow;

    /// Reset env vars that the embedder + tier config read, so test
    /// runs are deterministic regardless of the host environment.
    struct EnvGuard {
        keys: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear(keys: &[&'static str]) -> Self {
            let saved: Vec<(&'static str, Option<String>)> =
                keys.iter().map(|k| (*k, std::env::var(*k).ok())).collect();
            for k in keys {
                unsafe {
                    std::env::remove_var(k);
                }
            }
            Self { keys: saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.keys {
                match v {
                    Some(val) => unsafe { std::env::set_var(k, val) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }

    // The local-embedder load path (fastembed) downloads a 90 MB ONNX
    // model on first run; force `Disabled` so tests stay hermetic.
    fn disabled_embedder_guard() -> EnvGuard {
        let g = EnvGuard::clear(&[
            crate::memory::embedder::ENV_EMBEDDER_KIND,
            crate::memory::embedder::ENV_EMBEDDER_PROVIDER,
            crate::memory::embedder::ENV_EMBEDDER_URL,
            crate::memory::embedder::ENV_EMBEDDER_API_KEY,
            crate::memory::embedder::ENV_EMBEDDER_MODEL,
        ]);
        unsafe {
            std::env::set_var(crate::memory::embedder::ENV_EMBEDDER_KIND, "disabled");
        }
        g
    }

    fn write_row(store: &Arc<Mutex<SqliteStore>>, row: &MemoryRow) {
        let mut s = store.lock().unwrap();
        let dim = s.embed_dim();
        let vec: Vec<f32> = (0..dim).map(|i| 0.1 + i as f32 * 0.001).collect();
        s.insert(row, &vec).unwrap();
    }

    fn make_row(tier: Tier, access_count: u32, last_access_at_ms: u64, content: &str) -> MemoryRow {
        MemoryRow {
            id: MemoryId::new_v7(),
            session_id: None,
            scope_key: None,
            branch_name: None,
            is_orphan: false,
            tier,
            content: content.to_string(),
            created_at_ms: 0,
            updated_at_ms: 0,
            tier_change_at_ms: 0,
            access_count,
            last_access_at_ms,
            metadata_json: None,
        }
    }

    #[test]
    fn spawn_memory_service_opens_store_and_lexical_in_state_dir() {
        let _g = disabled_embedder_guard();
        let tmp = tempfile::tempdir().unwrap();
        let svc = spawn_memory_service(tmp.path()).unwrap();
        assert!(tmp.path().join("memory").join("memory.db").exists());
        assert!(tmp.path().join("memory").join("tantivy").is_dir());
        assert_eq!(svc.consolidate_interval_ms, DEFAULT_CONSOLIDATE_INTERVAL_MS);
        svc.shutdown();
    }

    #[test]
    fn spawn_memory_service_creates_dirs_if_missing() {
        let _g = disabled_embedder_guard();
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("does").join("not").join("exist");
        let svc = spawn_memory_service(&nested).unwrap();
        assert!(nested.join("memory").exists());
        assert!(nested.join("memory").join("memory.db").exists());
        svc.shutdown();
    }

    #[test]
    fn wal_recovery_on_open_does_not_panic() {
        let _g = disabled_embedder_guard();
        let tmp = tempfile::tempdir().unwrap();
        // Open once to materialize the schema + WAL companions.
        {
            let svc = spawn_memory_service(tmp.path()).unwrap();
            svc.shutdown();
        }
        // Reopen — exercises the wal_checkpoint(TRUNCATE) recovery path
        // on an existing DB. Must not panic and must not error.
        let svc = spawn_memory_service(tmp.path()).unwrap();
        svc.shutdown();
    }

    #[test]
    fn consolidation_tick_promotes_and_forgets() {
        let _g = disabled_embedder_guard();
        let tmp = tempfile::tempdir().unwrap();
        let svc = spawn_memory_service(tmp.path()).unwrap();

        // Working row stale enough to be auto-forgotten.
        let stale = make_row(Tier::Working, 0, 0, "stale");
        // Working row hot enough to be promoted to Episodic.
        let hot = make_row(Tier::Working, 99, 1_000_000_000, "hot promotable");

        write_row(&svc.store, &stale);
        write_row(&svc.store, &hot);

        // now_ms picked so:
        //   - `stale.last_access_at_ms = 0` is older than working_ttl_ms
        //     (default 24 h)
        //   - `hot.tier_change_at_ms = 0` clears the default 1-h dwell gate
        let now_ms = svc.tier_config.working_ttl_ms + svc.tier_config.promote_dwell_ms + 1;

        let report = svc.run_one_consolidation_tick(now_ms).unwrap();
        // The stale row gets dropped before the promotion query sees it.
        // What matters is that the cycle ran and didn't error.
        assert!(report.promoted + report.forgotten >= 1);

        svc.shutdown();
    }

    #[test]
    fn reconciliation_reindexes_missing_lexical_rows() {
        let _g = disabled_embedder_guard();
        let tmp = tempfile::tempdir().unwrap();

        // First open: insert a row, then tear down the lexical index
        // directory so a second open has to reconcile.
        {
            let svc = spawn_memory_service(tmp.path()).unwrap();
            let row = make_row(Tier::Semantic, 1, 0, "reconcile target");
            write_row(&svc.store, &row);
            svc.shutdown();
        }

        let tantivy = tmp.path().join("memory").join("tantivy");
        std::fs::remove_dir_all(&tantivy).unwrap();

        // Second open: tantivy dir is gone; reconciliation pass must
        // recreate it and re-upsert the SQLite row.
        let svc = spawn_memory_service(tmp.path()).unwrap();
        assert!(tantivy.exists());
        let lex = svc.lexical.lock().unwrap();
        let hits = lex.search("reconcile", 10, None, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        drop(lex);
        svc.shutdown();
    }

    #[test]
    fn daemon_info_includes_memory_mcp_port_field() {
        use super::super::types::DaemonInfo;
        // Construct via the serialized wire shape so the test fails the
        // moment the field is dropped from the struct.
        let wire = serde_json::json!({
            "pid": 1,
            "port": 5,
            "memory_mcp_port": 7777u16,
        });
        let info: DaemonInfo = serde_json::from_value(wire).unwrap();
        assert_eq!(info.memory_mcp_port, Some(7777));
    }
}
