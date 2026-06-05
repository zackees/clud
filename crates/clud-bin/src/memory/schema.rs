use rusqlite::Connection;

use crate::memory::error::MemoryError;

pub const TARGET_USER_VERSION: i32 = 1;

const SCHEMA_V1_TEMPLATE: &str = include_str!("../../schema/memory_v1.sql");

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

    // current < TARGET_USER_VERSION: apply the v1 migration.
    if current != 0 {
        return Err(MemoryError::Migration(format!(
            "unknown intermediate user_version={current}; only fresh (0) \
             databases can migrate to v{TARGET_USER_VERSION}"
        )));
    }

    let rendered = SCHEMA_V1_TEMPLATE.replace("{embed_dim}", &embed_dim.to_string());

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(&rendered)?;
    tx.execute(
        "INSERT INTO memory_meta(key, value) VALUES (?1, ?2)",
        rusqlite::params![EMBED_DIM_META_KEY, embed_dim.to_string()],
    )?;
    tx.pragma_update(None, "user_version", TARGET_USER_VERSION)?;
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
    fn migrate_fresh_file_sets_user_version_1() {
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
}
