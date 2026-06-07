use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::memory::error::MemoryError;
use crate::memory::ids::MemoryId;
use crate::memory::schema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Working,
    Episodic,
    Semantic,
}

impl Tier {
    pub fn as_i64(self) -> i64 {
        match self {
            Tier::Working => 0,
            Tier::Episodic => 1,
            Tier::Semantic => 2,
        }
    }

    pub fn from_i64(v: i64) -> Result<Self, MemoryError> {
        match v {
            0 => Ok(Tier::Working),
            1 => Ok(Tier::Episodic),
            2 => Ok(Tier::Semantic),
            other => Err(MemoryError::Migration(format!(
                "invalid tier discriminant {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRow {
    pub id: MemoryId,
    pub session_id: Option<String>,
    pub tier: Tier,
    pub content: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub tier_change_at_ms: u64,
    pub access_count: u32,
    pub last_access_at_ms: u64,
    pub metadata_json: Option<String>,
    /// Repo-level scope key produced by `memory::identity::scope_key`.
    /// `None` for global / pre-#267 rows (matches `session_id = NULL`
    /// semantics: null filters do not partition).
    pub scope_key: Option<String>,
    /// Current branch name when the row was recorded. Provenance metadata
    /// only; branch is NOT a partition key by default (DD-014).
    pub branch_name: Option<String>,
    /// True iff the recording branch was detected as an orphan branch
    /// at save time. Provenance only.
    pub is_orphan: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KnnHit {
    pub id: MemoryId,
    pub distance: f32,
}

pub struct SqliteStore {
    conn: Connection,
    embed_dim: usize,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore")
            .field("embed_dim", &self.embed_dim)
            .finish()
    }
}

static VEC_INIT_RESULT: OnceLock<Mutex<Result<(), String>>> = OnceLock::new();

// sqlite-vec ships a C extension that registers itself via the canonical
// sqlite3_auto_extension entrypoint. Registration is process-global and
// must happen before any Connection::open call that should see vec0.
fn ensure_vec_extension_loaded() -> Result<(), MemoryError> {
    type AutoExtInit = unsafe extern "C" fn(
        *mut rusqlite::ffi::sqlite3,
        *mut *const std::os::raw::c_char,
        *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;

    let cell = VEC_INIT_RESULT.get_or_init(|| {
        // SAFETY: sqlite3_auto_extension is the documented SQLite entry
        // point for registering an extension init function. sqlite-vec
        // exposes its init symbol; we transmute the bare `extern "C"`
        // pointer to the auto-extension's expected
        // (db, pzErrMsg, pApi) -> int signature. The underlying C
        // function ignores the arguments it doesn't use, matching how the
        // sqlite-vec README's rust example does the same cast.
        let rc = unsafe {
            let init_fn: AutoExtInit =
                std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
            rusqlite::ffi::sqlite3_auto_extension(Some(init_fn))
        };
        if rc == rusqlite::ffi::SQLITE_OK {
            Mutex::new(Ok(()))
        } else {
            Mutex::new(Err(format!("sqlite3_auto_extension returned {rc}")))
        }
    });
    let guard = cell.lock().expect("VEC_INIT_RESULT poisoned");
    match guard.as_ref() {
        Ok(()) => Ok(()),
        Err(msg) => Err(MemoryError::VecExtension(msg.clone())),
    }
}

impl SqliteStore {
    pub fn open(db_path: &Path, embed_dim: usize) -> Result<Self, MemoryError> {
        ensure_vec_extension_loaded()?;

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut conn = Connection::open_with_flags(db_path, flags)?;

        // WAL + FK on every open. Pragmas are connection-scoped (except
        // journal_mode which is persisted in the file header) so the
        // duplicate set on reopen is cheap and idempotent.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        schema::migrate(&mut conn, embed_dim)?;

        Ok(Self { conn, embed_dim })
    }

    pub fn embed_dim(&self) -> usize {
        self.embed_dim
    }

    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    // Issue #257: the embedder module's `reembed_all` walks rows then
    // rewrites the vec table. It owns its own transactions so it asks for
    // both a read-only handle (to enumerate ids+content) and a mutable
    // handle (to start `BEGIN IMMEDIATE` per row). These accessors keep
    // the connection encapsulated everywhere else.
    pub(crate) fn conn_ref(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    #[cfg(test)]
    pub(crate) fn fetch_vec_for_test(&self, id: &MemoryId) -> Result<Vec<f32>, MemoryError> {
        let blob: Vec<u8> = self.conn.query_row(
            "SELECT embedding FROM memory_vec WHERE id = ?1",
            params![id.as_str()],
            |r| r.get(0),
        )?;
        if blob.len() % 4 != 0 {
            return Err(MemoryError::Migration(format!(
                "vec blob length {} is not a multiple of 4",
                blob.len()
            )));
        }
        let mut out = Vec::with_capacity(blob.len() / 4);
        for chunk in blob.chunks_exact(4) {
            let bytes: [u8; 4] = chunk.try_into().unwrap();
            out.push(f32::from_le_bytes(bytes));
        }
        Ok(out)
    }

    pub fn insert(&mut self, row: &MemoryRow, embedding: &[f32]) -> Result<(), MemoryError> {
        if embedding.len() != self.embed_dim {
            return Err(MemoryError::DimMismatch {
                expected: self.embed_dim,
                got: embedding.len(),
            });
        }

        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        tx.execute(
            "INSERT INTO memories(
                id, session_id, tier, content,
                created_at_ms, updated_at_ms, tier_change_at_ms,
                access_count, last_access_at_ms, metadata_json,
                scope_key, branch_name, is_orphan
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                row.id.as_str(),
                row.session_id,
                row.tier.as_i64(),
                row.content,
                row.created_at_ms as i64,
                row.updated_at_ms as i64,
                row.tier_change_at_ms as i64,
                row.access_count as i64,
                row.last_access_at_ms as i64,
                row.metadata_json,
                row.scope_key,
                row.branch_name,
                row.is_orphan as i64,
            ],
        )?;

        let blob = embedding_blob(embedding);
        tx.execute(
            "INSERT INTO memory_vec(id, embedding) VALUES (?1, ?2)",
            params![row.id.as_str(), blob],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn fetch(&self, id: &MemoryId) -> Result<Option<MemoryRow>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, tier, content,
                    created_at_ms, updated_at_ms, tier_change_at_ms,
                    access_count, last_access_at_ms, metadata_json,
                    scope_key, branch_name, is_orphan
               FROM memories WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id.as_str()])?;
        if let Some(r) = rows.next()? {
            Ok(Some(row_from_sqlite(r)?))
        } else {
            Ok(None)
        }
    }

    pub fn fetch_many(&self, ids: &[MemoryId]) -> Result<Vec<MemoryRow>, MemoryError> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = self.fetch(id)? {
                out.push(row);
            }
        }
        Ok(out)
    }

    pub fn delete(&mut self, id: &MemoryId) -> Result<bool, MemoryError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let n = tx.execute("DELETE FROM memories WHERE id = ?1", params![id.as_str()])?;
        tx.execute("DELETE FROM memory_vec WHERE id = ?1", params![id.as_str()])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn list_by_tier(&self, tier: Tier) -> Result<Vec<MemoryRow>, MemoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, tier, content,
                    created_at_ms, updated_at_ms, tier_change_at_ms,
                    access_count, last_access_at_ms, metadata_json,
                    scope_key, branch_name, is_orphan
               FROM memories
              WHERE tier = ?1
              ORDER BY id",
        )?;
        let mut rows = stmt.query(params![tier.as_i64()])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(row_from_sqlite(r)?);
        }
        Ok(out)
    }

    pub fn promote_tier(
        &mut self,
        id: &MemoryId,
        to: Tier,
        now_ms: u64,
    ) -> Result<(), MemoryError> {
        let n = self.conn.execute(
            "UPDATE memories
                SET tier = ?2, tier_change_at_ms = ?3, updated_at_ms = ?3
              WHERE id = ?1",
            params![id.as_str(), to.as_i64(), now_ms as i64],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(id.clone()));
        }
        Ok(())
    }

    pub fn touch_access(&mut self, id: &MemoryId, now_ms: u64) -> Result<(), MemoryError> {
        let n = self.conn.execute(
            "UPDATE memories
                SET access_count = access_count + 1,
                    last_access_at_ms = ?2
              WHERE id = ?1",
            params![id.as_str(), now_ms as i64],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(id.clone()));
        }
        Ok(())
    }

    pub fn knn(
        &self,
        query: &[f32],
        k: usize,
        session_id: Option<&str>,
        tier_floor: Option<Tier>,
        scope_key: Option<&str>,
    ) -> Result<Vec<KnnHit>, MemoryError> {
        if query.len() != self.embed_dim {
            return Err(MemoryError::DimMismatch {
                expected: self.embed_dim,
                got: query.len(),
            });
        }

        // vec0 KNN: filter on the virtual table side first (cheapest), then
        // join against memories to apply session/tier/scope filters. The
        // join is done in SQL — vec0 only knows the embedding column.
        let mut sql = String::from(
            "SELECT v.id, v.distance
               FROM memory_vec v
               JOIN memories m ON m.id = v.id
              WHERE v.embedding MATCH ?1 AND k = ?2",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        params_vec.push(Box::new(embedding_blob(query)));
        params_vec.push(Box::new(k as i64));

        if let Some(sid) = session_id {
            sql.push_str(" AND m.session_id = ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(Box::new(sid.to_string()));
        }
        if let Some(floor) = tier_floor {
            sql.push_str(" AND m.tier >= ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(Box::new(floor.as_i64()));
        }
        if let Some(sk) = scope_key {
            sql.push_str(" AND m.scope_key = ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(Box::new(sk.to_string()));
        }
        sql.push_str(" ORDER BY v.distance ASC");

        let mut stmt = self.conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        let mut rows = stmt.query(refs.as_slice())?;
        let mut hits = Vec::new();
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            let dist: f64 = r.get(1)?;
            hits.push(KnnHit {
                id: MemoryId::parse(&id)?,
                distance: dist as f32,
            });
        }
        Ok(hits)
    }

    pub fn checkpoint_truncate(&mut self) -> Result<(), MemoryError> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}

fn embedding_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn row_from_sqlite(r: &rusqlite::Row<'_>) -> Result<MemoryRow, MemoryError> {
    let id: String = r.get(0)?;
    let session_id: Option<String> = r.get(1)?;
    let tier_i: i64 = r.get(2)?;
    let content: String = r.get(3)?;
    let created_at_ms: i64 = r.get(4)?;
    let updated_at_ms: i64 = r.get(5)?;
    let tier_change_at_ms: i64 = r.get(6)?;
    let access_count: i64 = r.get(7)?;
    let last_access_at_ms: i64 = r.get(8)?;
    let metadata_json: Option<String> = r.get(9)?;
    let scope_key: Option<String> = r.get(10)?;
    let branch_name: Option<String> = r.get(11)?;
    let is_orphan_i: i64 = r.get(12)?;
    Ok(MemoryRow {
        id: MemoryId::parse(&id)?,
        session_id,
        tier: Tier::from_i64(tier_i)?,
        content,
        created_at_ms: created_at_ms as u64,
        updated_at_ms: updated_at_ms as u64,
        tier_change_at_ms: tier_change_at_ms as u64,
        access_count: access_count as u32,
        last_access_at_ms: last_access_at_ms as u64,
        metadata_json,
        scope_key,
        branch_name,
        is_orphan: is_orphan_i != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: MemoryId, session_id: Option<&str>, tier: Tier, content: &str) -> MemoryRow {
        MemoryRow {
            id,
            session_id: session_id.map(|s| s.to_string()),
            tier,
            content: content.to_string(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
            tier_change_at_ms: 1_000,
            access_count: 0,
            last_access_at_ms: 1_000,
            metadata_json: None,
            scope_key: None,
            branch_name: None,
            is_orphan: false,
        }
    }

    fn row_scoped(
        id: MemoryId,
        session_id: Option<&str>,
        scope_key: Option<&str>,
        tier: Tier,
        content: &str,
    ) -> MemoryRow {
        MemoryRow {
            id,
            session_id: session_id.map(|s| s.to_string()),
            tier,
            content: content.to_string(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
            tier_change_at_ms: 1_000,
            access_count: 0,
            last_access_at_ms: 1_000,
            metadata_json: None,
            scope_key: scope_key.map(|s| s.to_string()),
            branch_name: None,
            is_orphan: false,
        }
    }

    fn vec384(seed: f32) -> Vec<f32> {
        (0..384).map(|i| seed + i as f32 * 0.001).collect()
    }

    #[test]
    fn open_creates_schema_at_target_user_version() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("memory.db");
        let store = SqliteStore::open(&db, 384).expect("open");
        let uv: i32 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            uv,
            crate::memory::schema::TARGET_USER_VERSION,
            "schema migration must reach target user_version"
        );
        let tables: Vec<String> = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for required in [
            "actions",
            "lessons",
            "memories",
            "memory_relations",
            "sessions",
        ] {
            assert!(tables.iter().any(|t| t == required), "missing {required}");
        }
        let vec_table: String = store
            .conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE name='memory_vec'",
                [],
                |r| r.get(0),
            )
            .expect("memory_vec virtual table must exist");
        assert_eq!(vec_table, "memory_vec");
    }

    #[test]
    fn insert_then_fetch_roundtrips_row() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id = MemoryId::new_v7();
        let row = row(id.clone(), Some("sess-a"), Tier::Working, "hello world");
        store.insert(&row, &vec384(0.1)).unwrap();
        let fetched = store.fetch(&id).unwrap().expect("row");
        assert_eq!(fetched, row);
    }

    #[test]
    fn insert_rejects_wrong_dim_embedding() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id = MemoryId::new_v7();
        let row = row(id, None, Tier::Working, "x");
        let err = store.insert(&row, &[0.0; 16]).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::DimMismatch {
                    expected: 384,
                    got: 16
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn delete_removes_from_memories_and_memory_vec() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id = MemoryId::new_v7();
        store
            .insert(&row(id.clone(), None, Tier::Working, "x"), &vec384(0.2))
            .unwrap();
        assert!(store.delete(&id).unwrap());
        let m_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE id = ?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        let v_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vec WHERE id = ?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(m_count, 0);
        assert_eq!(v_count, 0);
    }

    #[test]
    fn knn_filters_by_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id_a = MemoryId::new_v7();
        let id_b = MemoryId::new_v7();
        store
            .insert(
                &row(id_a.clone(), Some("A"), Tier::Working, "a"),
                &vec384(0.1),
            )
            .unwrap();
        store
            .insert(
                &row(id_b.clone(), Some("B"), Tier::Working, "b"),
                &vec384(0.2),
            )
            .unwrap();
        let hits = store.knn(&vec384(0.1), 10, Some("A"), None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id_a);
    }

    #[test]
    fn knn_filters_by_tier_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id_w = MemoryId::new_v7();
        let id_s = MemoryId::new_v7();
        store
            .insert(&row(id_w.clone(), None, Tier::Working, "w"), &vec384(0.1))
            .unwrap();
        store
            .insert(&row(id_s.clone(), None, Tier::Semantic, "s"), &vec384(0.11))
            .unwrap();
        let hits = store
            .knn(&vec384(0.1), 10, None, Some(Tier::Episodic), None)
            .unwrap();
        assert!(hits.iter().all(|h| h.id != id_w));
        assert!(hits.iter().any(|h| h.id == id_s));
    }

    #[test]
    fn knn_filters_by_scope_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id_a = MemoryId::new_v7();
        let id_b = MemoryId::new_v7();
        let id_global = MemoryId::new_v7();
        store
            .insert(
                &row_scoped(id_a.clone(), None, Some("repo://A"), Tier::Working, "a"),
                &vec384(0.1),
            )
            .unwrap();
        store
            .insert(
                &row_scoped(id_b.clone(), None, Some("repo://B"), Tier::Working, "b"),
                &vec384(0.11),
            )
            .unwrap();
        // A row with no scope at all (global): must not leak into a
        // scope-filtered query.
        store
            .insert(
                &row(id_global.clone(), None, Tier::Working, "g"),
                &vec384(0.12),
            )
            .unwrap();
        let hits = store
            .knn(&vec384(0.1), 10, None, None, Some("repo://A"))
            .unwrap();
        let ids: Vec<&MemoryId> = hits.iter().map(|h| &h.id).collect();
        assert!(
            ids.contains(&&id_a),
            "scoped query must include scope match"
        );
        assert!(
            !ids.contains(&&id_b),
            "scoped query must exclude other scope"
        );
        assert!(
            !ids.contains(&&id_global),
            "scoped query must exclude global rows"
        );
        // No scope filter → all rows visible.
        let all = store.knn(&vec384(0.1), 10, None, None, None).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn insert_with_scope_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id = MemoryId::new_v7();
        let mut written = row_scoped(
            id.clone(),
            Some("sess"),
            Some("repo://https://github.com/foo/bar"),
            Tier::Working,
            "hello",
        );
        written.branch_name = Some("feature/x".to_string());
        written.is_orphan = true;
        store.insert(&written, &vec384(0.5)).unwrap();
        let fetched = store.fetch(&id).unwrap().expect("row");
        assert_eq!(fetched, written);
    }

    #[test]
    fn promote_tier_updates_tier_and_tier_change_at() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id = MemoryId::new_v7();
        store
            .insert(&row(id.clone(), None, Tier::Working, "x"), &vec384(0.1))
            .unwrap();
        store.promote_tier(&id, Tier::Semantic, 9_999).unwrap();
        let fetched = store.fetch(&id).unwrap().unwrap();
        assert_eq!(fetched.tier, Tier::Semantic);
        assert_eq!(fetched.tier_change_at_ms, 9_999);
        assert_eq!(fetched.updated_at_ms, 9_999);
    }

    #[test]
    fn touch_access_increments_count_and_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id = MemoryId::new_v7();
        store
            .insert(&row(id.clone(), None, Tier::Working, "x"), &vec384(0.1))
            .unwrap();
        store.touch_access(&id, 4_242).unwrap();
        store.touch_access(&id, 5_555).unwrap();
        let fetched = store.fetch(&id).unwrap().unwrap();
        assert_eq!(fetched.access_count, 2);
        assert_eq!(fetched.last_access_at_ms, 5_555);
    }

    // Verifies the BEGIN IMMEDIATE wrapping by simulating a partial insert:
    // if memory_vec insert fails (here by feeding a duplicate id after a
    // first successful insert with the same id, which violates the PK),
    // the memories row must also be absent from the second attempt.
    #[test]
    fn insert_and_knn_share_one_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::open(&tmp.path().join("memory.db"), 384).unwrap();
        let id = MemoryId::new_v7();
        store
            .insert(&row(id.clone(), None, Tier::Working, "x"), &vec384(0.1))
            .unwrap();
        // Re-insert with same id: memories PK violates first, the whole tx
        // rolls back, memory_vec remains at exactly one row for this id.
        let err = store
            .insert(&row(id.clone(), None, Tier::Working, "y"), &vec384(0.2))
            .unwrap_err();
        assert!(matches!(err, MemoryError::Sqlite(_)));
        let v_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vec WHERE id = ?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v_count, 1, "rollback must not leak vec rows");
        let m_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE id = ?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(m_count, 1);
    }
}
