//! The broker's durable store: cached responses, the invalidation stamp and
//! the ledger, in one redb file under the daemon state dir (#1743).
//!
//! redb rather than SQLite: clud dropped its bundled SQLite for redb
//! (Cargo.toml, #73/#110), and only the daemon opens this file, which is
//! redb's single-writer model. See DD-150.

use std::path::Path;

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

/// cache key -> JSON [`ObjectMeta`].
const OBJECT_META: TableDefinition<&str, &[u8]> = TableDefinition::new("object_meta");
/// cache key -> raw response body.
const OBJECT_BODY: TableDefinition<&str, &[u8]> = TableDefinition::new("object_body");
/// `invalidated_at_ms` -> stamp of the last possible write.
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
/// monotonic sequence -> JSON [`LedgerEntry`].
const LEDGER: TableDefinition<u64, &[u8]> = TableDefinition::new("ledger");

const INVALIDATED_AT: &str = "invalidated_at_ms";
/// Cached objects kept before the oldest are pruned.
const MAX_OBJECTS: u64 = 4096;
const PRUNE_OBJECTS: usize = 512;
/// Ledger rows kept before the oldest are pruned.
const MAX_LEDGER: u64 = 20_000;
const PRUNE_LEDGER: usize = 2_000;

/// Everything about a cached response except its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectMeta {
    /// `<host or "">/<endpoint>`, for humans reading the store.
    pub label: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// When the upstream request that produced or last revalidated this
    /// body *started*, Unix ms. A fetch that began before an invalidation
    /// therefore never counts as fresh after it.
    pub fetched_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub ts_ms: u64,
    pub session_id: Option<String>,
    pub key: String,
    /// `cache`, `304`, `full`, `passthrough` or `error`.
    pub outcome: String,
    pub upstream_requests: u32,
    pub rate_remaining: Option<u64>,
}

pub struct Store {
    db: Database,
}

fn err(error: impl std::fmt::Display) -> String {
    format!("gh broker store: {error}")
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(err)?;
        }
        let db = Database::create(path).map_err(err)?;
        let txn = db.begin_write().map_err(err)?;
        {
            txn.open_table(OBJECT_META).map_err(err)?;
            txn.open_table(OBJECT_BODY).map_err(err)?;
            txn.open_table(META).map_err(err)?;
            txn.open_table(LEDGER).map_err(err)?;
        }
        txn.commit().map_err(err)?;
        Ok(Self { db })
    }

    pub fn get(&self, key: &str) -> Result<Option<(ObjectMeta, Vec<u8>)>, String> {
        let txn = self.db.begin_read().map_err(err)?;
        let metas = txn.open_table(OBJECT_META).map_err(err)?;
        let bodies = txn.open_table(OBJECT_BODY).map_err(err)?;
        let Some(meta) = metas.get(key).map_err(err)? else {
            return Ok(None);
        };
        let Some(body) = bodies.get(key).map_err(err)? else {
            return Ok(None);
        };
        let meta: ObjectMeta = serde_json::from_slice(meta.value()).map_err(err)?;
        Ok(Some((meta, body.value().to_vec())))
    }

    /// Insert or replace one object. `body` is `None` to keep the stored
    /// body (a `304` revalidation).
    pub fn put(&self, key: &str, meta: &ObjectMeta, body: Option<&[u8]>) -> Result<(), String> {
        let bytes = serde_json::to_vec(meta).map_err(err)?;
        let txn = self.db.begin_write().map_err(err)?;
        {
            let mut metas = txn.open_table(OBJECT_META).map_err(err)?;
            let mut bodies = txn.open_table(OBJECT_BODY).map_err(err)?;
            metas.insert(key, bytes.as_slice()).map_err(err)?;
            if let Some(body) = body {
                bodies.insert(key, body).map_err(err)?;
            }
            if metas.len().map_err(err)? > MAX_OBJECTS {
                let mut ages = Vec::new();
                for row in metas.iter().map_err(err)? {
                    let (k, v) = row.map_err(err)?;
                    let at = serde_json::from_slice::<ObjectMeta>(v.value())
                        .map(|m| m.fetched_at_ms)
                        .unwrap_or(0);
                    ages.push((at, k.value().to_string()));
                }
                ages.sort();
                for (_, old) in ages.into_iter().take(PRUNE_OBJECTS) {
                    if old != key {
                        metas.remove(old.as_str()).map_err(err)?;
                        bodies.remove(old.as_str()).map_err(err)?;
                    }
                }
            }
        }
        txn.commit().map_err(err)
    }

    pub fn invalidated_at(&self) -> Result<u64, String> {
        let txn = self.db.begin_read().map_err(err)?;
        let meta = txn.open_table(META).map_err(err)?;
        Ok(meta
            .get(INVALIDATED_AT)
            .map_err(err)?
            .map(|v| v.value())
            .unwrap_or(0))
    }

    pub fn invalidate(&self, now_ms: u64) -> Result<(), String> {
        let txn = self.db.begin_write().map_err(err)?;
        {
            let mut meta = txn.open_table(META).map_err(err)?;
            meta.insert(INVALIDATED_AT, now_ms).map_err(err)?;
        }
        txn.commit().map_err(err)
    }

    pub fn append_ledger(&self, entry: &LedgerEntry) -> Result<(), String> {
        let bytes = serde_json::to_vec(entry).map_err(err)?;
        let mut txn = self.db.begin_write().map_err(err)?;
        // The ledger is diagnostics: no fsync on the read path.
        txn.set_durability(redb::Durability::Eventual);
        {
            let mut ledger = txn.open_table(LEDGER).map_err(err)?;
            let next = ledger
                .last()
                .map_err(err)?
                .map(|(k, _)| k.value() + 1)
                .unwrap_or(0);
            ledger.insert(next, bytes.as_slice()).map_err(err)?;
            if ledger.len().map_err(err)? > MAX_LEDGER {
                let mut old = Vec::new();
                for row in ledger.iter().map_err(err)?.take(PRUNE_LEDGER) {
                    old.push(row.map_err(err)?.0.value());
                }
                for seq in old {
                    ledger.remove(seq).map_err(err)?;
                }
            }
        }
        txn.commit().map_err(err)
    }

    pub fn ledger(&self) -> Result<Vec<LedgerEntry>, String> {
        let txn = self.db.begin_read().map_err(err)?;
        let ledger = txn.open_table(LEDGER).map_err(err)?;
        let mut out = Vec::new();
        for row in ledger.iter().map_err(err)? {
            let (_, v) = row.map_err(err)?;
            out.push(serde_json::from_slice(v.value()).map_err(err)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(at: u64) -> ObjectMeta {
        ObjectMeta {
            label: "/repos/o/r".into(),
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            etag: Some("\"e1\"".into()),
            last_modified: None,
            fetched_at_ms: at,
        }
    }

    #[test]
    fn objects_survive_reopen_and_a_304_keeps_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.redb");
        {
            let store = Store::open(&path).unwrap();
            store.put("k", &meta(1), Some(b"{\"a\":1}")).unwrap();
            store.put("k", &meta(2), None).unwrap();
            store.invalidate(7).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let (m, body) = store.get("k").unwrap().unwrap();
        assert_eq!(m.fetched_at_ms, 2);
        assert_eq!(body, b"{\"a\":1}");
        assert_eq!(store.invalidated_at().unwrap(), 7);
        assert!(store.get("missing").unwrap().is_none());
    }

    #[test]
    fn ledger_appends_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("b.redb")).unwrap();
        for outcome in ["full", "cache", "304"] {
            store
                .append_ledger(&LedgerEntry {
                    ts_ms: 1,
                    session_id: None,
                    key: "k".into(),
                    outcome: outcome.into(),
                    upstream_requests: 0,
                    rate_remaining: None,
                })
                .unwrap();
        }
        let outcomes: Vec<_> = store
            .ledger()
            .unwrap()
            .into_iter()
            .map(|e| e.outcome)
            .collect();
        assert_eq!(outcomes, ["full", "cache", "304"]);
    }
}
