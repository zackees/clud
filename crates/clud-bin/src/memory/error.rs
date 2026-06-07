use crate::memory::ids::MemoryId;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("sqlite-vec load failed: {0}")]
    VecExtension(String),
    // tantivy errors are stringified at the boundary: a few internal variants
    // (notably `TantivyError::IndexAlreadyExists` wrappers) are not
    // `Send + 'static`, which breaks downstream `Result<_, MemoryError>` use
    // in worker threads.
    #[error("tantivy: {0}")]
    Tantivy(String),
    #[error("schema migration: {0}")]
    Migration(String),
    #[error("vector dim mismatch: expected {expected}, got {got}")]
    DimMismatch { expected: usize, got: usize },
    #[error("not found: {0}")]
    NotFound(MemoryId),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
