//! Issue #264: git-artifact serialization for the agent-memory store.
//!
//! Writes the semantic (and optionally episodic) tier as a tree of
//! human-readable Markdown files under `<git-root>/.clud/memory/`. Each
//! file is `<ulid>-<slug>.md` with a YAML frontmatter block followed by
//! the raw memory body. A `.cludignore` file at the same root provides
//! conservative defaults for excluding secrets via:
//!
//! - shell-style glob patterns matching the row's `scope_key`,
//!   `session_id`, or relative path,
//! - `body-regex:` lines matching against the body text,
//! - `private: true` in the row's `metadata_json`, which always wins.
//!
//! Tier-gated visibility (DD-016, [`tier_exportable`]):
//!
//! - Working — never exported.
//! - Episodic — opt-in via `CLUD_MEMORY_EXPORT_EPISODIC=1` or
//!   `--include-episodic` on the CLI.
//! - Semantic — always exported by default.
//!
//! See the module's section in `memory/README.md` and the
//! "Git-artifact serialization" subsection in
//! `docs/architecture/memory.md` for the file-layout and rationale.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::memory::embedder::EmbedderTrait;
use crate::memory::error::MemoryError;
use crate::memory::ids::MemoryId;
use crate::memory::lexical::LexicalIndex;
use crate::memory::store::{MemoryRow, SqliteStore, Tier};
use crate::memory::tiers::{tier_exportable, TierConfig};

/// Subdirectory name (relative to the resolved git root) where the
/// git-artifact tree lives.
pub const MEMORY_DIR_REL: &str = ".clud/memory";
/// Filename of the privacy filter inside [`MEMORY_DIR_REL`].
pub const CLUDIGNORE_FILENAME: &str = ".cludignore";
/// Filename of the append-only relations log inside [`MEMORY_DIR_REL`].
pub const RELATIONS_FILENAME: &str = "relations.jsonl";

/// Resolve `<git_root>/.clud/memory/`.
pub fn memory_dir(git_root: &Path) -> PathBuf {
    git_root.join(".clud").join("memory")
}

/// Knobs passed to [`export_to_disk`] / [`import_from_disk`].
#[derive(Debug, Clone)]
pub struct DiskArtifactConfig {
    pub root: PathBuf,
    pub include_episodic: bool,
    pub allow_private: bool,
}

impl DiskArtifactConfig {
    /// Build a default config rooted at `<git_root>/.clud/memory/`. Reads
    /// `CLUD_MEMORY_EXPORT_EPISODIC` for the tier-policy override.
    pub fn at(git_root: &Path) -> Self {
        Self {
            root: memory_dir(git_root),
            include_episodic: std::env::var("CLUD_MEMORY_EXPORT_EPISODIC")
                .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false),
            allow_private: false,
        }
    }
}

/// Compiled privacy filter loaded from `<root>/.cludignore`.
#[derive(Debug)]
pub struct PrivacyFilter {
    cludignore_globs: Vec<String>,
    body_regex_blacklist: Vec<regex::Regex>,
}

impl PrivacyFilter {
    /// Build an empty filter (no globs, no regexes). Useful for tests
    /// and as the fallback when the file is absent.
    pub fn empty() -> Self {
        Self {
            cludignore_globs: Vec::new(),
            body_regex_blacklist: Vec::new(),
        }
    }

    /// Load `<root>/.cludignore` if it exists; return an empty filter
    /// when the file is missing.
    pub fn load_from_root(root: &Path) -> Result<Self, MemoryError> {
        let path = root.join(CLUDIGNORE_FILENAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(MemoryError::Io(e)),
        }
    }

    /// Parse a `.cludignore` text body. Lines starting with `#` are
    /// comments; blank lines are skipped. Lines starting with
    /// `body-regex:` are compiled as regexes; everything else is a
    /// shell-style glob applied to the row's `scope_key`, `session_id`,
    /// or path-like metadata fields.
    pub fn parse(text: &str) -> Result<Self, MemoryError> {
        let mut globs = Vec::new();
        let mut regexes = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("body-regex:") {
                let pattern = rest.trim();
                if pattern.is_empty() {
                    continue;
                }
                let re = regex::Regex::new(pattern).map_err(|e| {
                    MemoryError::CludIgnore(format!("invalid body-regex `{pattern}`: {e}"))
                })?;
                regexes.push(re);
            } else {
                globs.push(line.to_string());
            }
        }
        Ok(Self {
            cludignore_globs: globs,
            body_regex_blacklist: regexes,
        })
    }

    /// Returns `true` when `row` should be excluded from on-disk export.
    /// Honors body regexes, then scope/session globs, then the
    /// `private: true` frontmatter override.
    pub fn should_skip(&self, row: &MemoryRow) -> bool {
        if metadata_has_private_true(row.metadata_json.as_deref()) {
            return true;
        }
        for re in &self.body_regex_blacklist {
            if re.is_match(&row.content) {
                return true;
            }
        }
        for glob in &self.cludignore_globs {
            if let Some(sk) = row.scope_key.as_deref() {
                if glob_matches(glob, sk) {
                    return true;
                }
            }
            if let Some(sid) = row.session_id.as_deref() {
                if glob_matches(glob, sid) {
                    return true;
                }
            }
        }
        false
    }
}

/// Best-effort check for `"private": true` in a row's `metadata_json`.
/// Unparseable JSON is treated as "no `private` flag".
fn metadata_has_private_true(metadata: Option<&str>) -> bool {
    let Some(raw) = metadata else {
        return false;
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => v.get("private").and_then(|x| x.as_bool()).unwrap_or(false),
        Err(_) => false,
    }
}

/// Aggregate counters from [`export_to_disk`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub written: usize,
    pub skipped_privacy: usize,
    pub skipped_tier: usize,
    pub relations_appended: usize,
}

/// Aggregate counters from [`import_from_disk`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped_existing: usize,
    pub errors: usize,
    pub relations_replayed: usize,
}

/// YAML frontmatter shape persisted at the top of every `.md` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    pub id: String,
    pub tier: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub scope_key: Option<String>,
    #[serde(default)]
    pub branch_name: Option<String>,
    #[serde(default)]
    pub is_orphan: bool,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub tier_change_at_ms: u64,
    pub access_count: u32,
    pub last_access_at_ms: u64,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub private: bool,
}

impl Frontmatter {
    fn from_row(row: &MemoryRow) -> Self {
        let metadata = match row.metadata_json.as_deref() {
            Some(raw) => serde_json::from_str(raw).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        };
        let private = metadata
            .get("private")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            id: row.id.as_str().to_string(),
            tier: tier_token(row.tier).to_string(),
            session_id: row.session_id.clone(),
            scope_key: row.scope_key.clone(),
            branch_name: row.branch_name.clone(),
            is_orphan: row.is_orphan,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
            tier_change_at_ms: row.tier_change_at_ms,
            access_count: row.access_count,
            last_access_at_ms: row.last_access_at_ms,
            metadata,
            private,
        }
    }

    fn to_row(&self, content: String) -> Result<MemoryRow, MemoryError> {
        let tier = parse_tier_token(&self.tier)?;
        let id = MemoryId::parse(&self.id)?;
        let metadata_json = if matches!(self.metadata, serde_json::Value::Null) {
            None
        } else {
            Some(self.metadata.to_string())
        };
        Ok(MemoryRow {
            id,
            session_id: self.session_id.clone(),
            tier,
            content,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            tier_change_at_ms: self.tier_change_at_ms,
            access_count: self.access_count,
            last_access_at_ms: self.last_access_at_ms,
            metadata_json,
            scope_key: self.scope_key.clone(),
            branch_name: self.branch_name.clone(),
            is_orphan: self.is_orphan,
        })
    }
}

/// One row of the append-only `relations.jsonl` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationRecord {
    pub src_id: String,
    pub dst_id: String,
    pub kind: String,
    pub weight: f32,
    pub created_at_ms: u64,
}

fn tier_token(tier: Tier) -> &'static str {
    match tier {
        Tier::Working => "working",
        Tier::Episodic => "episodic",
        Tier::Semantic => "semantic",
    }
}

fn parse_tier_token(s: &str) -> Result<Tier, MemoryError> {
    match s {
        "working" => Ok(Tier::Working),
        "episodic" => Ok(Tier::Episodic),
        "semantic" => Ok(Tier::Semantic),
        other => Err(MemoryError::Frontmatter(format!(
            "unknown tier token: {other}"
        ))),
    }
}

fn tier_subdir(tier: Tier) -> Option<&'static str> {
    match tier {
        Tier::Working => None,
        Tier::Episodic => Some("episodic"),
        Tier::Semantic => Some("semantic"),
    }
}

/// Build a deterministic slug from `content` for use in a filename. Up
/// to 40 chars of `[a-z0-9]` after lowercasing and collapsing other
/// runs to single `-`. Empty result falls back to `memory`.
fn slug_from_content(content: &str) -> String {
    let first_line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let limited: String = first_line.chars().take(80).collect();
    let mut out = String::with_capacity(40);
    let mut last_dash = false;
    for c in limited.chars() {
        let c_lc = c.to_ascii_lowercase();
        if c_lc.is_ascii_alphanumeric() {
            out.push(c_lc);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        return "memory".to_string();
    }
    out
}

/// Filename prefix used for sortability. We mint a fresh ULID per
/// export so the on-disk listing groups by export time; the canonical
/// id stays in the frontmatter (uuidv7).
fn ulid_prefix() -> String {
    ulid::Ulid::new().to_string()
}

fn artifact_filename(row: &MemoryRow) -> String {
    format!("{}-{}.md", ulid_prefix(), slug_from_content(&row.content))
}

fn ensure_dirs(cfg: &DiskArtifactConfig) -> Result<(), MemoryError> {
    std::fs::create_dir_all(cfg.root.join("semantic"))?;
    if cfg.include_episodic {
        std::fs::create_dir_all(cfg.root.join("episodic"))?;
    }
    Ok(())
}

/// Render a row as a YAML frontmatter block followed by the body.
fn render_artifact(row: &MemoryRow) -> Result<String, MemoryError> {
    let fm = Frontmatter::from_row(row);
    let yaml = serde_yaml::to_string(&fm)
        .map_err(|e| MemoryError::Frontmatter(format!("serialize: {e}")))?;
    let mut out = String::with_capacity(yaml.len() + row.content.len() + 16);
    out.push_str("---\n");
    out.push_str(&yaml);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(&row.content);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Split a Markdown artifact into `(frontmatter, body)`. Accepts CRLF
/// or LF line endings.
pub fn split_frontmatter(text: &str) -> Result<(Frontmatter, String), MemoryError> {
    let normalized = text.replace("\r\n", "\n");
    let trimmed = normalized.trim_start_matches('\u{feff}');
    let rest = trimmed.strip_prefix("---\n").ok_or_else(|| {
        MemoryError::Frontmatter("missing opening `---` frontmatter delimiter".into())
    })?;
    let close = rest
        .find("\n---\n")
        .or_else(|| rest.strip_suffix("\n---").map(|_| rest.len() - 4));
    let close =
        close.ok_or_else(|| MemoryError::Frontmatter("missing closing `---` delimiter".into()))?;
    let yaml = &rest[..close];
    let body_start = close + "\n---\n".len();
    let body = if body_start >= rest.len() {
        String::new()
    } else {
        // Strip the blank line we inject after the closing delimiter
        // and the single trailing newline we add when serializing.
        let raw = rest[body_start..].trim_start_matches('\n');
        raw.strip_suffix('\n').unwrap_or(raw).to_string()
    };
    let fm: Frontmatter = serde_yaml::from_str(yaml)
        .map_err(|e| MemoryError::Frontmatter(format!("parse yaml: {e}")))?;
    Ok((fm, body))
}

/// Atomic write: write to a sibling temp file, then rename into place.
fn atomic_write(target: &Path, contents: &str) -> Result<(), MemoryError> {
    let parent = target
        .parent()
        .ok_or_else(|| MemoryError::Io(std::io::Error::other("target has no parent directory")))?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.flush()?;
    tmp.persist(target).map_err(|e| MemoryError::Io(e.error))?;
    Ok(())
}

/// Walk every exportable row in `store` and write one Markdown artifact
/// per row under `<cfg.root>/<tier>/`. Honors the `.cludignore` filter
/// loaded from `<cfg.root>/.cludignore` and the `private: true`
/// metadata override.
pub fn export_to_disk(
    store: &SqliteStore,
    cfg: &DiskArtifactConfig,
) -> Result<ExportReport, MemoryError> {
    ensure_dirs(cfg)?;
    let filter = PrivacyFilter::load_from_root(&cfg.root)?;
    let tier_cfg = TierConfig {
        episodic_exportable: cfg.include_episodic,
        ..TierConfig::default()
    };

    let mut report = ExportReport::default();
    let existing_ids = scan_existing_ids(&cfg.root)?;

    for tier in [Tier::Working, Tier::Episodic, Tier::Semantic] {
        if !tier_exportable(tier, &tier_cfg) {
            report.skipped_tier += store.list_by_tier(tier)?.len();
            continue;
        }
        let Some(subdir) = tier_subdir(tier) else {
            // Tier-exportable above already returns false for Working;
            // this branch is unreachable in practice but kept defensive.
            continue;
        };
        let dest = cfg.root.join(subdir);
        for row in store.list_by_tier(tier)? {
            if existing_ids.contains(row.id.as_str()) {
                continue;
            }
            if !cfg.allow_private && filter.should_skip(&row) {
                report.skipped_privacy += 1;
                continue;
            }
            let body = render_artifact(&row)?;
            let path = dest.join(artifact_filename(&row));
            atomic_write(&path, &body)?;
            report.written += 1;
        }
    }

    report.relations_appended = append_new_relations(store, &cfg.root)?;
    Ok(report)
}

/// Walk every `*.md` under `<cfg.root>/semantic` (and `episodic/` when
/// the policy is widened) and re-insert any rows not already present.
/// Re-embeds via `embedder` and writes through `lexical`. Replays
/// `relations.jsonl` into the `memory_relations` table.
pub fn import_from_disk(
    store: &mut SqliteStore,
    lexical: &mut LexicalIndex,
    embedder: &impl EmbedderTrait,
    cfg: &DiskArtifactConfig,
) -> Result<ImportReport, MemoryError> {
    let mut report = ImportReport::default();
    if !cfg.root.exists() {
        return Ok(report);
    }
    let tiers: &[&str] = if cfg.include_episodic {
        &["semantic", "episodic"]
    } else {
        &["semantic"]
    };
    for subdir in tiers {
        let dir = cfg.root.join(subdir);
        if !dir.exists() {
            continue;
        }
        let entries = std::fs::read_dir(&dir)?;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    report.errors += 1;
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            match import_single_file(store, lexical, embedder, &path) {
                Ok(ImportOutcome::Inserted) => report.imported += 1,
                Ok(ImportOutcome::Existing) => report.skipped_existing += 1,
                Err(_) => report.errors += 1,
            }
        }
    }
    lexical.commit()?;
    report.relations_replayed = replay_relations(store, &cfg.root)?;
    Ok(report)
}

enum ImportOutcome {
    Inserted,
    Existing,
}

fn import_single_file(
    store: &mut SqliteStore,
    lexical: &mut LexicalIndex,
    embedder: &impl EmbedderTrait,
    path: &Path,
) -> Result<ImportOutcome, MemoryError> {
    let text = std::fs::read_to_string(path)?;
    let (fm, body) = split_frontmatter(&text)?;
    let row = fm.to_row(body)?;
    if store.fetch(&row.id)?.is_some() {
        return Ok(ImportOutcome::Existing);
    }
    let embedding = embedder.embed(&row.content)?;
    store.insert(&row, &embedding)?;
    lexical.upsert(
        &row.id,
        row.session_id.as_deref(),
        row.scope_key.as_deref(),
        row.tier,
        &row.content,
    )?;
    Ok(ImportOutcome::Inserted)
}

/// Append any rows from `memory_relations` not yet recorded in
/// `relations.jsonl`. Idempotent: rereading the file detects dupes by
/// `(src_id, dst_id, kind)` and skips them.
fn append_new_relations(store: &SqliteStore, root: &Path) -> Result<usize, MemoryError> {
    let conn = store.conn_ref();
    let table_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_relations'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "SELECT src_id, dst_id, kind, COALESCE(weight, 1.0), COALESCE(created_at_ms, 0)
           FROM memory_relations
          ORDER BY created_at_ms ASC",
    )?;
    let mut rows = stmt.query([])?;
    let mut records: Vec<RelationRecord> = Vec::new();
    while let Some(r) = rows.next()? {
        let src_id: String = r.get(0)?;
        let dst_id: String = r.get(1)?;
        let kind: String = r.get(2)?;
        let weight: f64 = r.get(3)?;
        let created_at_ms: i64 = r.get(4)?;
        records.push(RelationRecord {
            src_id,
            dst_id,
            kind,
            weight: weight as f32,
            created_at_ms: created_at_ms as u64,
        });
    }
    drop(rows);
    drop(stmt);

    let path = root.join(RELATIONS_FILENAME);
    let existing = read_existing_relation_keys(&path);
    let mut appended = 0usize;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for rec in records {
        let key = (rec.src_id.clone(), rec.dst_id.clone(), rec.kind.clone());
        if existing.contains(&key) {
            continue;
        }
        let line = serde_json::to_string(&rec).map_err(|e| {
            MemoryError::Frontmatter(format!("relations.jsonl serialize failed: {e}"))
        })?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        appended += 1;
    }
    Ok(appended)
}

fn read_existing_relation_keys(path: &Path) -> HashSet<(String, String, String)> {
    let mut out = HashSet::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<RelationRecord>(trimmed) {
            out.insert((rec.src_id, rec.dst_id, rec.kind));
        }
    }
    out
}

fn replay_relations(store: &mut SqliteStore, root: &Path) -> Result<usize, MemoryError> {
    let path = root.join(RELATIONS_FILENAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(MemoryError::Io(e)),
    };
    let conn = store.conn_mut();
    let table_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_relations'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(0);
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut count = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rec: RelationRecord = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO memory_relations(src_id, dst_id, kind, weight, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                rec.src_id,
                rec.dst_id,
                rec.kind,
                rec.weight as f64,
                rec.created_at_ms as i64,
            ],
        )?;
        if inserted > 0 {
            count += 1;
        }
    }
    tx.commit()?;
    Ok(count)
}

/// Walk `<root>/{semantic,episodic}/*.md` and collect every parsed
/// MemoryId. Used to skip already-exported rows on a subsequent run.
fn scan_existing_ids(root: &Path) -> Result<HashSet<String>, MemoryError> {
    let mut ids = HashSet::new();
    for subdir in ["semantic", "episodic"] {
        let dir = root.join(subdir);
        if !dir.exists() {
            continue;
        }
        let entries = std::fs::read_dir(&dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok((fm, _)) = split_frontmatter(&text) {
                    ids.insert(fm.id);
                }
            }
        }
    }
    Ok(ids)
}

/// Minimal shell-style glob: `*` matches any run, `?` matches one char,
/// otherwise literal. No path semantics — scope keys and session ids
/// aren't paths.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_rec(&p, 0, &t, 0)
}

fn glob_rec(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    match p[pi] {
        '*' => {
            let mut np = pi;
            while np < p.len() && p[np] == '*' {
                np += 1;
            }
            if np == p.len() {
                return true;
            }
            for next_ti in ti..=t.len() {
                if glob_rec(p, np, t, next_ti) {
                    return true;
                }
            }
            false
        }
        '?' => {
            if ti == t.len() {
                false
            } else {
                glob_rec(p, pi + 1, t, ti + 1)
            }
        }
        c => ti < t.len() && t[ti] == c && glob_rec(p, pi + 1, t, ti + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::TestEmbedder;
    use crate::memory::ids::MemoryId;
    use crate::memory::lexical::LexicalIndex;

    fn vec_n(seed: f32, dim: usize) -> Vec<f32> {
        (0..dim).map(|i| seed + i as f32 * 0.001).collect()
    }

    fn make_row(tier: Tier, content: &str) -> MemoryRow {
        MemoryRow {
            id: MemoryId::new_v7(),
            session_id: Some("s1".to_string()),
            scope_key: Some("repo://example.com/foo/bar".to_string()),
            branch_name: Some("main".to_string()),
            is_orphan: false,
            tier,
            content: content.to_string(),
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_000_000,
            tier_change_at_ms: 1_700_000_000_000,
            access_count: 3,
            last_access_at_ms: 1_700_000_000_000,
            metadata_json: None,
        }
    }

    fn make_row_with_metadata(tier: Tier, content: &str, metadata_json: &str) -> MemoryRow {
        let mut row = make_row(tier, content);
        row.metadata_json = Some(metadata_json.to_string());
        row
    }

    fn fresh_store(tmp: &tempfile::TempDir) -> SqliteStore {
        SqliteStore::open(&tmp.path().join("memory.db"), 8).unwrap()
    }

    fn fresh_lex(tmp: &tempfile::TempDir) -> LexicalIndex {
        LexicalIndex::open_or_create(&tmp.path().join("tantivy")).unwrap()
    }

    fn insert(store: &mut SqliteStore, row: &MemoryRow, seed: f32) {
        store.insert(row, &vec_n(seed, 8)).unwrap();
    }

    fn default_cfg(root: PathBuf) -> DiskArtifactConfig {
        DiskArtifactConfig {
            root,
            include_episodic: false,
            allow_private: false,
        }
    }

    #[test]
    fn export_writes_yaml_frontmatter_and_markdown_body() {
        let db = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&db);
        let semantic = make_row(Tier::Semantic, "Auth uses HS256 JWTs");
        insert(&mut store, &semantic, 0.10);

        let out = tempfile::tempdir().unwrap();
        let cfg = default_cfg(out.path().to_path_buf());
        let report = export_to_disk(&store, &cfg).unwrap();
        assert_eq!(report.written, 1);
        assert_eq!(report.skipped_privacy, 0);

        let files: Vec<_> = std::fs::read_dir(cfg.root.join("semantic"))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(files.len(), 1);
        let path = files[0].path();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\n"), "missing opening delimiter");
        assert!(text.contains("---\n\n"), "missing closing delimiter");
        assert!(
            text.contains(&format!("id: {}", semantic.id.as_str())),
            "frontmatter must include id"
        );
        assert!(text.contains("tier: semantic"));
        assert!(text.trim_end().ends_with("Auth uses HS256 JWTs"));
    }

    #[test]
    fn export_skips_working_tier_by_default() {
        let db = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&db);
        insert(&mut store, &make_row(Tier::Working, "scratch"), 0.10);
        insert(&mut store, &make_row(Tier::Semantic, "permanent"), 0.20);

        let out = tempfile::tempdir().unwrap();
        let cfg = default_cfg(out.path().to_path_buf());
        let report = export_to_disk(&store, &cfg).unwrap();
        assert_eq!(report.written, 1);
        assert!(
            report.skipped_tier >= 1,
            "working should be skipped by tier"
        );
        // No `working/` directory should be created.
        assert!(!cfg.root.join("working").exists());
    }

    #[test]
    fn export_includes_episodic_when_policy_set() {
        let db = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&db);
        insert(&mut store, &make_row(Tier::Episodic, "session note"), 0.10);
        insert(&mut store, &make_row(Tier::Semantic, "fact"), 0.20);

        let out = tempfile::tempdir().unwrap();
        let mut cfg = default_cfg(out.path().to_path_buf());
        cfg.include_episodic = true;
        let report = export_to_disk(&store, &cfg).unwrap();
        assert_eq!(report.written, 2);
        assert!(cfg.root.join("semantic").is_dir());
        assert!(cfg.root.join("episodic").is_dir());
    }

    #[test]
    fn export_skips_private_metadata_row() {
        let db = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&db);
        let private = make_row_with_metadata(Tier::Semantic, "secret note", r#"{"private":true}"#);
        let public = make_row(Tier::Semantic, "visible note");
        insert(&mut store, &private, 0.10);
        insert(&mut store, &public, 0.20);

        let out = tempfile::tempdir().unwrap();
        let cfg = default_cfg(out.path().to_path_buf());
        let report = export_to_disk(&store, &cfg).unwrap();
        assert_eq!(report.written, 1, "the private row must be skipped");
        assert_eq!(report.skipped_privacy, 1);
    }

    #[test]
    fn export_skips_row_matching_cludignore_body_regex() {
        let db = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&db);
        insert(
            &mut store,
            &make_row(Tier::Semantic, "AKIAEXAMPLE0123456789 leak"),
            0.10,
        );
        insert(
            &mut store,
            &make_row(Tier::Semantic, "Benign architectural note"),
            0.20,
        );

        let out = tempfile::tempdir().unwrap();
        let cfg = default_cfg(out.path().to_path_buf());
        std::fs::create_dir_all(&cfg.root).unwrap();
        std::fs::write(
            cfg.root.join(CLUDIGNORE_FILENAME),
            "body-regex: AKIA[0-9A-Z]{16}\n",
        )
        .unwrap();
        let report = export_to_disk(&store, &cfg).unwrap();
        assert_eq!(report.written, 1, "secret row must be filtered");
        assert_eq!(report.skipped_privacy, 1);
    }

    #[test]
    fn import_roundtrips_a_single_file() {
        let db1 = tempfile::tempdir().unwrap();
        let mut store1 = fresh_store(&db1);
        let row = make_row(Tier::Semantic, "Auth uses HS256 JWTs");
        insert(&mut store1, &row, 0.30);
        let disk = tempfile::tempdir().unwrap();
        let cfg = default_cfg(disk.path().to_path_buf());
        export_to_disk(&store1, &cfg).unwrap();

        // Fresh store + lexical + embedder.
        let db2 = tempfile::tempdir().unwrap();
        let mut store2 = fresh_store(&db2);
        let mut lex = fresh_lex(&db2);
        let embedder = TestEmbedder::with_dim(8);
        let report = import_from_disk(&mut store2, &mut lex, &embedder, &cfg).unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped_existing, 0);
        let fetched = store2.fetch(&row.id).unwrap().expect("row roundtripped");
        assert_eq!(fetched.tier, Tier::Semantic);
        assert_eq!(fetched.content, "Auth uses HS256 JWTs");
    }

    #[test]
    fn import_skips_files_with_existing_id() {
        let db1 = tempfile::tempdir().unwrap();
        let mut store1 = fresh_store(&db1);
        let row = make_row(Tier::Semantic, "existing");
        insert(&mut store1, &row, 0.40);
        let disk = tempfile::tempdir().unwrap();
        let cfg = default_cfg(disk.path().to_path_buf());
        export_to_disk(&store1, &cfg).unwrap();

        // Import into the SAME store: row is already there so the
        // import path skips without erroring.
        let mut lex = fresh_lex(&db1);
        let embedder = TestEmbedder::with_dim(8);
        let report = import_from_disk(&mut store1, &mut lex, &embedder, &cfg).unwrap();
        assert_eq!(report.imported, 0);
        assert_eq!(report.skipped_existing, 1);
    }

    #[test]
    fn relations_jsonl_appends_new_edges_only() {
        let db = tempfile::tempdir().unwrap();
        let mut store = fresh_store(&db);
        let a = make_row(Tier::Semantic, "a");
        let b = make_row(Tier::Semantic, "b");
        insert(&mut store, &a, 0.10);
        insert(&mut store, &b, 0.20);
        {
            let conn = store.conn_ref();
            conn.execute(
                "INSERT INTO memory_relations(src_id, dst_id, kind, weight, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![a.id.as_str(), b.id.as_str(), "refines", 1.0_f64, 1_i64],
            )
            .unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        let cfg = default_cfg(out.path().to_path_buf());
        let r1 = export_to_disk(&store, &cfg).unwrap();
        assert_eq!(r1.relations_appended, 1);
        let r2 = export_to_disk(&store, &cfg).unwrap();
        assert_eq!(r2.relations_appended, 0, "no new edges on second run");
        let body = std::fs::read_to_string(cfg.root.join(RELATIONS_FILENAME)).unwrap();
        assert_eq!(body.lines().count(), 1);
    }

    #[test]
    fn cludignore_parser_handles_globs_and_body_regex_lines() {
        let text = r"
# comment
*github.com/secret-repo*
body-regex: password\s*=

# another comment
session-xyz-*
";
        let filter = PrivacyFilter::parse(text).unwrap();
        let mut row = make_row(Tier::Semantic, "password = hunter2");
        row.scope_key = Some("not-matched".into());
        row.session_id = None;
        assert!(filter.should_skip(&row), "body regex must match");

        let mut row2 = make_row(Tier::Semantic, "harmless");
        row2.scope_key = Some("repo://github.com/secret-repo/x".into());
        row2.session_id = None;
        assert!(filter.should_skip(&row2), "scope glob must match");

        let mut row3 = make_row(Tier::Semantic, "harmless body");
        row3.scope_key = None;
        row3.session_id = Some("session-xyz-1".into());
        assert!(filter.should_skip(&row3), "session glob must match");

        let mut row4 = make_row(Tier::Semantic, "harmless body");
        row4.scope_key = Some("repo://github.com/public/x".into());
        row4.session_id = Some("session-abc".into());
        assert!(!filter.should_skip(&row4), "innocuous row must pass");
    }

    #[test]
    fn cludignore_invalid_regex_returns_error_not_panic() {
        let err = PrivacyFilter::parse("body-regex: (unbalanced\n").unwrap_err();
        assert!(matches!(err, MemoryError::CludIgnore(_)), "got {err:?}");
    }

    #[test]
    fn split_frontmatter_handles_crlf_and_lf() {
        let lf = "---\nid: abc\ntier: semantic\ncreated_at_ms: 1\nupdated_at_ms: 1\ntier_change_at_ms: 1\naccess_count: 0\nlast_access_at_ms: 1\nis_orphan: false\nprivate: false\nmetadata: null\n---\n\nbody-lf\n";
        let crlf = lf.replace('\n', "\r\n");
        let (fm_lf, body_lf) = split_frontmatter(lf).unwrap();
        let (fm_crlf, body_crlf) = split_frontmatter(&crlf).unwrap();
        assert_eq!(fm_lf.id, "abc");
        assert_eq!(fm_crlf.id, "abc");
        assert_eq!(body_lf.trim(), "body-lf");
        assert_eq!(body_crlf.trim(), "body-lf");
    }

    #[test]
    fn slug_truncates_and_falls_back_to_memory_literal() {
        assert_eq!(slug_from_content(""), "memory");
        assert_eq!(slug_from_content("   \n\t"), "memory");
        assert_eq!(slug_from_content("Hello World"), "hello-world");
        let long = "a".repeat(200);
        let slug = slug_from_content(&long);
        assert!(slug.len() <= 40);
        assert!(!slug.contains(' '));
    }
}
