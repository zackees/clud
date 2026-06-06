//! Agent-memory storage + hybrid search foundation.
//!
//! This module is the storage layer only — embedder, tier lifecycle, MCP
//! server, daemon IPC, and CLI verbs all live in sibling sub-issues under
//! META #255. The seams are clean: `SqliteStore` takes raw `&[f32]` slices,
//! `promote_tier` is a SQL primitive without retention policy, and
//! `LexicalIndex::upsert` accepts an explicit `Tier` instead of inferring
//! one.

pub mod cli;
pub mod embedder;
pub mod error;
pub mod git_artifact;
pub mod identity;
pub mod ids;
pub mod lexical;
pub mod mcp_config;
pub mod paths;
pub mod schema;
pub mod search;
pub mod store;
pub mod tiers;

pub use embedder::{
    embedder_from_env, reembed_all, Embedder, EmbedderTrait, RemoteEmbedder, RemoteProvider,
    EMBED_DIM_MINILM_L6_V2,
};
pub use error::MemoryError;
pub use git_artifact::{
    export_to_disk, import_from_disk, memory_dir as git_artifact_memory_dir, DiskArtifactConfig,
    ExportReport, Frontmatter, ImportReport, PrivacyFilter, RelationRecord, CLUDIGNORE_FILENAME,
    MEMORY_DIR_REL, RELATIONS_FILENAME,
};
pub use identity::{
    branch_isolate, branch_unisolate, cross_repo_glob_filter, normalize_origin_url,
    resolve_repo_scope, scope_key, RepoScope, BRANCH_ISOLATE_MARKER,
};
pub use ids::MemoryId;
pub use lexical::{LexicalHit, LexicalIndex};
pub use search::{rrf_fuse, FusedHit, HybridSearchConfig};
pub use store::{KnnHit, MemoryRow, SqliteStore, Tier};
pub use tiers::{
    apply_promotions, forget_expired, promote_candidates, retention_score, tier_exportable,
    TierConfig,
};
