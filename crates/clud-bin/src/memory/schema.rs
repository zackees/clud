use rusqlite::Connection;

use crate::memory::error::MemoryError;

pub const TARGET_USER_VERSION: i32 = 2;

const SCHEMA_V1_TEMPLATE: &str = include_str!("../../schema/memory_v1.sql");
const SCHEMA_V2_DELTA: &str = include_str!("../../schema/memory_v2.sql");

const EMBED_DIM_META_KEY: &str = "embed_dim";

pub fn migrate(conn: &mut Connection, embed_dim: usize) -> Result<(), MemoryError> {
    let current: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current == TARGET_USER_VERSION {
        let stored = read_stored_embed_dim(conn)?;
        if stored != embed_dim {
            return Err(MemoryError::DimMismatch {
                expected: stored,
                got: embed_dim,
            });
        }
        return Ok(());
    }

    if current > TARGET_USER_VERSION {
        return Err(MemoryError::Migration(format!(
            "database user_version={current} is newer than this binary supports \
             ({TARGET_USER_VERSION}); refusing to downgrade"
        )));
    }

    // Forward-only sequence: 0 → 1 → 2. Each step runs in its own
    // BEGIN IMMEDIATE so partial-failure semantics are well-defined.
    if current == 0 {
        apply_v1(conn, embed_dim)?;
    }
    let after_v1: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if after_v1 == 1 {
        apply_v2(conn)?;
    }

    // Validate the dim on whichever path got us here.
    let stored = read_stored_embed_dim(conn)?;
    if stored != embed_dim {
        return Err(MemoryError::DimMismatch {
            expected: stored,
            got: embed_dim,
        });
    }

    Ok(())
}

fn apply_v1(conn: &mut Connection, embed_dim: usize) -> Result<(), MemoryError> {
    let rendered = SCHEMA_V1_TEMPLATE.replace("{embed_dim}", &embed_dim.to_string());

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(&rendered)?;
    tx.execute(
        "INSERT INTO memory_meta(key, value) VALUES (?1, ?2)",
        rusqlite::params![EMBED_DIM_META_KEY, embed_dim.to_string()],
    )?;
    tx.pragma_update(None, "user_version", 1)?;
    tx.commit()?;
    Ok(())
}

fn apply_v2(conn: &mut Connection) -> Result<(), MemoryError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA_V2_DELTA)?;
    tx.pragma_update(None, "user_version", 2)?;
    tx.commit()?;
    Ok(())
}

fn read_stored_embed_dim(conn: &Connection) -> Result<usize, MemoryError> {
    let raw: String = conn
        .query_row(
            "SELECT value FROM memory_meta WHERE key = ?1",
            rusqlite::params![EMBED_DIM_META_KEY],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                MemoryError::Migration("memory_meta.embed_dim missing from v1 database".into())
            }
            other => MemoryError::Sqlite(other),
        })?;
    raw.parse::<usize>().map_err(|e| {
        MemoryError::Migration(format!(
            "memory_meta.embed_dim={raw:?} is not a valid usize: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::store::SqliteStore;

    // schema-level tests reach through SqliteStore::open because the
    // sqlite-vec extension must be auto-registered before connection open
    // for the CREATE VIRTUAL TABLE to succeed.
    fn fresh_db(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        tmp.path().join("memory.db")
    }

    #[test]
    fn migrate_fresh_file_sets_user_version_to_target() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh_db(&tmp);
        let store = SqliteStore::open(&db, 384).unwrap();
        let uv: i32 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, TARGET_USER_VERSION);
    }

    #[test]
    fn migrate_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh_db(&tmp);
        {
            let _store = SqliteStore::open(&db, 384).unwrap();
        }
        // Reopen with the same dim: must not error and must not double-apply.
        let store = SqliteStore::open(&db, 384).unwrap();
        let uv: i32 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, TARGET_USER_VERSION);
        let count: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM memory_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "memory_meta must not be re-seeded");
    }

    #[test]
    fn migrate_refuses_future_user_version() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh_db(&tmp);
        {
            let store = SqliteStore::open(&db, 384).unwrap();
            store
                .conn()
                .pragma_update(None, "user_version", 99i32)
                .unwrap();
        }
        let err = SqliteStore::open(&db, 384).unwrap_err();
        assert!(matches!(err, MemoryError::Migration(_)), "got {err:?}");
    }

    #[test]
    fn embed_dim_is_baked_into_vec_table() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh_db(&tmp);
        {
            let _store = SqliteStore::open(&db, 768).unwrap();
        }
        let err = SqliteStore::open(&db, 384).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::DimMismatch {
                    expected: 768,
                    got: 384
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn migrate_from_v1_to_v2_adds_scope_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh_db(&tmp);
        // Build a v1 DB by hand: bring it to v1 then forcibly downgrade
        // user_version so the migrator runs the v2 step on the next open.
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            // sqlite-vec is registered globally by other tests in this
            // process; ensure_vec_extension_loaded is idempotent via
            // OnceLock, but we still need to call it here in case this
            // test runs first.
            let _ = SqliteStore::open(&db, 384).unwrap();
            // Force the file back to v1 so we re-run the v2 step.
            conn.pragma_update(None, "user_version", 1i32).unwrap();
            // Drop the v2 columns/index we just added so the v2 ALTERs
            // don't fail with "duplicate column" when re-applied.
            conn.execute("DROP INDEX IF EXISTS idx_memories_scope", [])
                .unwrap();
            // SQLite < 3.35 lacks DROP COLUMN. Recreate the table without
            // the v2 columns.
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE memories_v1 (
                   id              TEXT PRIMARY KEY,
                   session_id      TEXT,
                   tier            INTEGER NOT NULL DEFAULT 0,
                   content         TEXT NOT NULL,
                   created_at_ms       INTEGER NOT NULL,
                   updated_at_ms       INTEGER NOT NULL,
                   tier_change_at_ms   INTEGER NOT NULL,
                   access_count        INTEGER NOT NULL DEFAULT 0,
                   last_access_at_ms   INTEGER NOT NULL,
                   metadata_json   TEXT
                 ) STRICT;
                 INSERT INTO memories_v1 SELECT
                   id, session_id, tier, content,
                   created_at_ms, updated_at_ms, tier_change_at_ms,
                   access_count, last_access_at_ms, metadata_json
                   FROM memories;
                 DROP TABLE memories;
                 ALTER TABLE memories_v1 RENAME TO memories;
                 CREATE INDEX idx_memories_session ON memories(session_id);
                 CREATE INDEX idx_memories_tier    ON memories(tier);
                 CREATE INDEX idx_memories_updated ON memories(updated_at_ms);
                 COMMIT;",
            )
            .unwrap();
            drop(conn);
        }
        // Reopen via the store — schema::migrate should drive v1 → v2.
        let store = SqliteStore::open(&db, 384).unwrap();
        let uv: i32 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 2);
        let columns: Vec<String> = store
            .conn()
            .prepare("PRAGMA table_info(memories)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for required in ["scope_key", "branch_name", "is_orphan"] {
            assert!(
                columns.iter().any(|c| c == required),
                "missing {required} after v1→v2"
            );
        }
        let idx_count: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_memories_scope'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 1, "idx_memories_scope must exist");
    }

    #[test]
    fn migrate_from_v0_to_v2_runs_both_migrations() {
        let tmp = tempfile::tempdir().unwrap();
        let db = fresh_db(&tmp);
        // Fresh DB: zero → 2 in one open.
        let store = SqliteStore::open(&db, 384).unwrap();
        let uv: i32 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 2, "fresh open must land on v2 directly");
        // Sanity: the v1 + v2 surface both exist.
        let columns: Vec<String> = store
            .conn()
            .prepare("PRAGMA table_info(memories)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for required in [
            "id",
            "session_id",
            "tier",
            "content",
            "scope_key",
            "branch_name",
            "is_orphan",
        ] {
            assert!(columns.iter().any(|c| c == required), "missing {required}");
        }
    }
}
