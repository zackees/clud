//! Issue #259: agent-memory MCP server, embedded in the clud daemon.
//!
//! Exposes the 8 ESSENTIAL_TOOLS (`memory_save`, `memory_recall`,
//! `memory_smart_search`, `memory_sessions`, `memory_consolidate`,
//! `memory_diagnose`, `memory_lesson_save`, `memory_reflect`) over
//! line-delimited JSON-RPC 2.0 on a loopback TCP port. The `clud mcp`
//! subcommand bridges Claude Code / Codex stdio to this listener so MCP
//! clients can save and recall memories across sessions.
//!
//! Protocol: each TCP connection is a JSON-RPC 2.0 channel. Requests and
//! responses are NDJSON (one JSON object per line). Supported methods:
//! `initialize`, `tools/list`, `tools/call`. Tool results return the
//! `{ content: [{type: "text", text: "<json-string>"}] }` shape mandated
//! by the MCP spec.
//!
//! Concurrency: one `std::thread` per connection (matches the rest of the
//! daemon's std::thread model — see DD-017 and DD-018). The
//! `Arc<MemoryService>` is cheap to clone; per-resource access is gated
//! by its existing `Mutex`es.
//!
//! Out of scope here: auto-registration of `~/.claude.json` /
//! `~/.codex/config.toml` (#265). Dashboard JS (#263). PreToolUse hook
//! capture path (#260).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
#[cfg(test)]
use serde::Serialize;
use serde_json::{json, Value};

use crate::memory::embedder::EmbedderTrait;
use crate::memory::ids::MemoryId;
use crate::memory::store::{MemoryRow, Tier};
use crate::memory::{rrf_fuse, HybridSearchConfig, MemoryError};

use super::memory_service::{run_one_consolidation_tick, MemoryService};

/// JSON-RPC 2.0 error codes — match the values agentmemory ships with.
const JSONRPC_PARSE_ERROR: i64 = -32700;
const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const JSONRPC_INTERNAL_ERROR: i64 = -32603;
/// Spec-mentioned "daemon unavailable" code used by the bridge when
/// `DaemonInfo.memory_mcp_port` is `None`.
pub const JSONRPC_DAEMON_UNAVAILABLE: i64 = -32099;

/// MCP protocol version we advertise. Matches the version Claude Code's
/// MCP host currently uses for negotiation.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Public handle returned by [`spawn_mcp_server`]. The port is what gets
/// written into `DaemonInfo.memory_mcp_port`.
pub struct McpServer {
    pub port: u16,
    /// Set to `true` to ask the accept loop to exit between connections.
    /// Tests use this for clean shutdown; production daemons just exit
    /// the process (the OS reclaims the listener).
    shutdown: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl McpServer {
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// Spawn the MCP server on an ephemeral loopback TCP port. The accept
/// loop runs on a single `std::thread`; each accepted connection spawns
/// its own per-connection thread. Returns the bound port (or a
/// `MemoryError::Io` if the bind itself failed).
pub fn spawn_mcp_server(memory: Arc<MemoryService>) -> Result<McpServer, MemoryError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_thread = Arc::clone(&shutdown);

    thread::Builder::new()
        .name("clud-memory-mcp".to_string())
        .spawn(move || accept_loop(listener, memory, shutdown_for_thread))
        .map_err(|err| {
            MemoryError::Io(std::io::Error::other(format!(
                "memory mcp accept loop spawn failed: {err}"
            )))
        })?;

    Ok(McpServer { port, shutdown })
}

fn accept_loop(listener: TcpListener, memory: Arc<MemoryService>, shutdown: Arc<AtomicBool>) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                let memory = Arc::clone(&memory);
                let _ = thread::Builder::new()
                    .name("clud-memory-mcp-conn".to_string())
                    .spawn(move || {
                        if let Err(err) = handle_connection(stream, memory) {
                            eprintln!("[clud] memory mcp connection error: {err}");
                        }
                    });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(err) => {
                eprintln!("[clud] memory mcp accept failed: {err}");
                thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn handle_connection(
    stream: std::net::TcpStream,
    memory: Arc<MemoryService>,
) -> std::io::Result<()> {
    let read_stream = stream.try_clone()?;
    let mut reader = BufReader::new(read_stream);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = process_request_line(trimmed, &memory);
        if let Some(resp) = response {
            let mut out = serde_json::to_vec(&resp)?;
            out.push(b'\n');
            writer.write_all(&out)?;
            writer.flush()?;
        }
    }
}

/// Parse one NDJSON request line and produce one NDJSON response value
/// (or `None` for notifications which intentionally have no response).
fn process_request_line(line: &str, memory: &Arc<MemoryService>) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(err) => {
            return Some(json_error(
                Value::Null,
                JSONRPC_PARSE_ERROR,
                format!("parse error: {err}"),
            ));
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = request.get("id").is_none();

    let method = match request.get("method").and_then(|v| v.as_str()) {
        Some(m) => m.to_string(),
        None => {
            if is_notification {
                return None;
            }
            return Some(json_error(
                id,
                JSONRPC_INVALID_REQUEST,
                "missing method".to_string(),
            ));
        }
    };
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    let result = dispatch_method(&method, params, memory);
    if is_notification {
        return None;
    }
    Some(match result {
        Ok(value) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value,
        }),
        Err(McpError { code, message }) => json_error(id, code, message),
    })
}

fn json_error(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[derive(Debug)]
struct McpError {
    code: i64,
    message: String,
}

impl McpError {
    fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: JSONRPC_INVALID_PARAMS,
            message: msg.into(),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: JSONRPC_INTERNAL_ERROR,
            message: msg.into(),
        }
    }
}

impl From<MemoryError> for McpError {
    fn from(err: MemoryError) -> Self {
        Self::internal(format!("memory error: {err}"))
    }
}

fn dispatch_method(
    method: &str,
    params: Value,
    memory: &Arc<MemoryService>,
) -> Result<Value, McpError> {
    match method {
        "initialize" => Ok(initialize_response()),
        "tools/list" => Ok(tools_list_response()),
        "tools/call" => dispatch_tool_call(params, memory),
        // Notifications (no `id`) are handled upstream; unknown
        // request methods come through here.
        _ => Err(McpError {
            code: JSONRPC_METHOD_NOT_FOUND,
            message: format!("unknown method: {method}"),
        }),
    }
}

fn initialize_response() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "clud-memory",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn tools_list_response() -> Value {
    json!({ "tools": tool_descriptors() })
}

fn dispatch_tool_call(params: Value, memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| McpError::invalid_params("missing tool name"))?
        .to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let body = match name.as_str() {
        "memory_save" => tool_memory_save(arguments, memory)?,
        "memory_recall" => tool_memory_recall(arguments, memory)?,
        "memory_smart_search" => tool_memory_smart_search(arguments, memory)?,
        "memory_sessions" => tool_memory_sessions(memory)?,
        "memory_consolidate" => tool_memory_consolidate(memory)?,
        "memory_diagnose" => tool_memory_diagnose(memory)?,
        "memory_lesson_save" => tool_memory_lesson_save(arguments, memory)?,
        "memory_reflect" => tool_memory_reflect()?,
        other => {
            return Err(McpError::invalid_params(format!("unknown tool: {other}")));
        }
    };
    let text = serde_json::to_string(&body)
        .map_err(|err| McpError::internal(format!("tool result serialization: {err}")))?;
    Ok(json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

// ------------------------------------------------------------------
// Tool descriptors — 1:1 with agentmemory's ESSENTIAL_TOOLS schemas.
// ------------------------------------------------------------------

fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "memory_save",
            "description": "Explicitly save an important insight, decision, or pattern to long-term memory.",
            "inputSchema": {
                "type": "object",
                "required": ["content"],
                "properties": {
                    "content": { "type": "string" },
                    "tier": { "type": "string", "enum": ["working", "episodic", "semantic"] },
                    "session_id": { "type": ["string", "null"] },
                    "metadata": { "type": ["object", "null"] }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "memory_recall",
            "description": "Fetch a single memory row by id.",
            "inputSchema": {
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "memory_smart_search",
            "description": "Hybrid RRF search over BM25 (tantivy) + vector (sqlite-vec).",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "k": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "session_id": { "type": ["string", "null"] },
                    "tier_floor": { "type": ["string", "null"], "enum": ["working", "episodic", "semantic", null] },
                    "scope_key": { "type": ["string", "null"] }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "memory_sessions",
            "description": "List distinct session_ids found in the memory store.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "memory_consolidate",
            "description": "Run the tier consolidation pipeline once and report the counts.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "memory_diagnose",
            "description": "Report basic memory subsystem health (embedder, db path, dim, row count, schema version).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "memory_lesson_save",
            "description": "Save a lesson learned from this session. Optionally link to a memory row.",
            "inputSchema": {
                "type": "object",
                "required": ["content"],
                "properties": {
                    "content": { "type": "string" },
                    "summary": { "type": ["string", "null"] },
                    "memory_id": { "type": ["string", "null"] }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "memory_reflect",
            "description": "Knowledge-graph reflection — documented stub in v0.1; lands fully in v0.5.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
    ]
}

// ------------------------------------------------------------------
// Tool handlers.
// ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SaveArgs {
    content: String,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
}

fn parse_tier(s: &str) -> Result<Tier, McpError> {
    match s.to_ascii_lowercase().as_str() {
        "working" => Ok(Tier::Working),
        "episodic" => Ok(Tier::Episodic),
        "semantic" => Ok(Tier::Semantic),
        other => Err(McpError::invalid_params(format!(
            "invalid tier `{other}`: expected one of working|episodic|semantic"
        ))),
    }
}

fn tool_memory_save(args: Value, memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let parsed: SaveArgs = serde_json::from_value(args)
        .map_err(|err| McpError::invalid_params(format!("invalid arguments: {err}")))?;
    if parsed.content.trim().is_empty() {
        return Err(McpError::invalid_params("content is required"));
    }
    let tier = match parsed.tier.as_deref() {
        Some(t) => parse_tier(t)?,
        None => Tier::Working,
    };
    let metadata_json = parsed
        .metadata
        .as_ref()
        .map(|m| {
            serde_json::to_string(m)
                .map_err(|err| McpError::internal(format!("metadata serialize: {err}")))
        })
        .transpose()?;
    let now_ms = unix_millis_now();
    let id = MemoryId::new_v7();
    let row = MemoryRow {
        id: id.clone(),
        session_id: parsed.session_id.clone(),
        tier,
        content: parsed.content.clone(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        tier_change_at_ms: now_ms,
        access_count: 0,
        last_access_at_ms: now_ms,
        metadata_json,
        scope_key: None,
        branch_name: None,
        is_orphan: false,
    };

    let vec = match memory.embedder.embed(&parsed.content) {
        Ok(v) => v,
        Err(MemoryError::EmbedderDisabled(_)) => {
            // Embedder disabled: write a zero vector of the stored dim so
            // the lexical index still picks the row up. KNN won't surface
            // it usefully, but smart_search's BM25 leg will.
            let dim = {
                let s = memory
                    .store
                    .lock()
                    .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
                s.embed_dim()
            };
            vec![0.0_f32; dim]
        }
        Err(err) => return Err(McpError::from(err)),
    };

    {
        let mut s = memory
            .store
            .lock()
            .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
        s.insert(&row, &vec)?;
    }
    {
        let mut l = memory
            .lexical
            .lock()
            .map_err(|_| McpError::internal("memory lexical mutex poisoned"))?;
        l.upsert(
            &row.id,
            row.session_id.as_deref(),
            row.scope_key.as_deref(),
            row.tier,
            &row.content,
        )?;
        l.commit()?;
    }
    Ok(json!({ "id": id.as_str() }))
}

#[derive(Debug, Deserialize)]
struct RecallArgs {
    id: String,
}

fn tool_memory_recall(args: Value, memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let parsed: RecallArgs = serde_json::from_value(args)
        .map_err(|err| McpError::invalid_params(format!("invalid arguments: {err}")))?;
    let id = MemoryId::parse(&parsed.id)
        .map_err(|err| McpError::invalid_params(format!("invalid id: {err}")))?;
    let row = {
        let s = memory
            .store
            .lock()
            .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
        s.fetch(&id)?
    };
    match row {
        Some(r) => Ok(memory_row_to_json(&r)),
        None => Err(McpError {
            code: JSONRPC_INVALID_PARAMS,
            message: format!("memory `{}` not found", parsed.id),
        }),
    }
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    k: Option<usize>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    tier_floor: Option<String>,
    #[serde(default)]
    scope_key: Option<String>,
}

fn tool_memory_smart_search(args: Value, memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let parsed: SearchArgs = serde_json::from_value(args)
        .map_err(|err| McpError::invalid_params(format!("invalid arguments: {err}")))?;
    if parsed.query.trim().is_empty() {
        return Err(McpError::invalid_params("query is required"));
    }
    let k = parsed.k.unwrap_or(10).clamp(1, 100);
    let tier_floor = parsed.tier_floor.as_deref().map(parse_tier).transpose()?;

    let bm25_hits = {
        let l = memory
            .lexical
            .lock()
            .map_err(|_| McpError::internal("memory lexical mutex poisoned"))?;
        // Tantivy's QueryParser can choke on punctuation-heavy inputs; if
        // the parse fails, treat as zero BM25 hits rather than aborting
        // the whole search.
        l.search(
            &parsed.query,
            k,
            parsed.session_id.as_deref(),
            tier_floor,
            parsed.scope_key.as_deref(),
        )
        .unwrap_or_default()
    };

    let vec_hits = match memory.embedder.embed(&parsed.query) {
        Ok(v) => {
            let s = memory
                .store
                .lock()
                .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
            // Dim drift between the embedder and the stored vec0 column
            // is a soft error here — fall back to lexical-only ranking.
            if v.len() == s.embed_dim() {
                s.knn(
                    &v,
                    k,
                    parsed.session_id.as_deref(),
                    tier_floor,
                    parsed.scope_key.as_deref(),
                )
                .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        Err(_) => Vec::new(),
    };

    let fused = rrf_fuse(&bm25_hits, &vec_hits, &HybridSearchConfig::from_env());

    let ids: Vec<MemoryId> = fused.iter().take(k).map(|h| h.id.clone()).collect();
    let rows = {
        let s = memory
            .store
            .lock()
            .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
        s.fetch_many(&ids)?
    };

    let results: Vec<Value> = rows.iter().map(memory_row_to_json).collect();
    Ok(json!({
        "results": results,
        "total": results.len(),
    }))
}

fn tool_memory_sessions(memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let s = memory
        .store
        .lock()
        .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
    let conn = s.conn_ref();
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT session_id FROM memories \
             WHERE session_id IS NOT NULL ORDER BY session_id",
        )
        .map_err(|err| McpError::internal(format!("prepare: {err}")))?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|err| McpError::internal(format!("query: {err}")))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(json!({ "sessions": rows }))
}

fn tool_memory_consolidate(memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let now_ms = unix_millis_now();
    let report =
        run_one_consolidation_tick(&memory.store, &memory.lexical, &memory.tier_config, now_ms)?;
    Ok(json!({
        "promoted": report.promoted,
        "forgotten": report.forgotten,
    }))
}

fn tool_memory_diagnose(memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let embed_dim = <crate::memory::Embedder as EmbedderTrait>::dim(memory.embedder.as_ref());
    let embedder_name =
        <crate::memory::Embedder as EmbedderTrait>::name(memory.embedder.as_ref()).to_string();
    let s = memory
        .store
        .lock()
        .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
    let stored_dim = s.embed_dim();
    let conn = s.conn_ref();
    let row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap_or(0);
    let schema_user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0);
    Ok(json!({
        "embedder": embedder_name,
        "embed_dim": embed_dim,
        "stored_embed_dim": stored_dim,
        "row_count": row_count,
        "schema_user_version": schema_user_version,
        "notes": "v0.1 diagnose surface — extended subsystem checks land later",
    }))
}

#[derive(Debug, Deserialize)]
struct LessonSaveArgs {
    content: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    memory_id: Option<String>,
}

fn tool_memory_lesson_save(args: Value, memory: &Arc<MemoryService>) -> Result<Value, McpError> {
    let parsed: LessonSaveArgs = serde_json::from_value(args)
        .map_err(|err| McpError::invalid_params(format!("invalid arguments: {err}")))?;
    if parsed.content.trim().is_empty() {
        return Err(McpError::invalid_params("content is required"));
    }
    let id = MemoryId::new_v7();
    let now_ms = unix_millis_now();
    let summary = parsed.summary.unwrap_or_else(|| parsed.content.clone());
    let s = memory
        .store
        .lock()
        .map_err(|_| McpError::internal("memory store mutex poisoned"))?;
    s.conn_ref()
        .execute(
            "INSERT INTO lessons(id, memory_id, summary, created_at_ms, metadata_json) \
             VALUES (?1, ?2, ?3, ?4, NULL)",
            rusqlite::params![id.as_str(), parsed.memory_id, summary, now_ms as i64],
        )
        .map_err(|err| McpError::internal(format!("insert lesson: {err}")))?;
    Ok(json!({ "id": id.as_str() }))
}

fn tool_memory_reflect() -> Result<Value, McpError> {
    // Spec calls this out as a documented stub in v0.1; the full
    // implementation lands in v0.5 (knowledge-graph + LLM provider).
    Err(McpError {
        code: JSONRPC_INTERNAL_ERROR,
        message: "memory_reflect is not yet implemented (lands in v0.5)".to_string(),
    })
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn memory_row_to_json(row: &MemoryRow) -> Value {
    json!({
        "id": row.id.as_str(),
        "session_id": row.session_id,
        "tier": tier_label(row.tier),
        "content": row.content,
        "created_at_ms": row.created_at_ms,
        "updated_at_ms": row.updated_at_ms,
        "tier_change_at_ms": row.tier_change_at_ms,
        "access_count": row.access_count,
        "last_access_at_ms": row.last_access_at_ms,
        "metadata_json": row.metadata_json,
        "scope_key": row.scope_key,
        "branch_name": row.branch_name,
        "is_orphan": row.is_orphan,
    })
}

fn tier_label(t: Tier) -> &'static str {
    match t {
        Tier::Working => "working",
        Tier::Episodic => "episodic",
        Tier::Semantic => "semantic",
    }
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ------------------------------------------------------------------
// Wire-level helpers used by the bridge + tests
// ------------------------------------------------------------------

/// A minimal JSON-RPC 2.0 request struct used by the in-process tests.
#[cfg(test)]
#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub params: Value,
}

#[cfg(test)]
impl<'a> JsonRpcRequest<'a> {
    pub fn new(id: u64, method: &'a str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::memory_service::spawn_memory_service;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Duration;

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

    fn spawn_service_and_server() -> (tempfile::TempDir, Arc<MemoryService>, McpServer) {
        let _g = disabled_embedder_guard();
        let tmp = tempfile::tempdir().unwrap();
        let svc = Arc::new(spawn_memory_service(tmp.path()).unwrap());
        let server = spawn_mcp_server(Arc::clone(&svc)).unwrap();
        // give the accept loop a beat to enter `listener.accept`.
        std::thread::sleep(Duration::from_millis(20));
        (tmp, svc, server)
    }

    fn send_request(port: u16, req: &Value) -> Value {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let line = format!("{}\n", req);
        stream.write_all(line.as_bytes()).unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        serde_json::from_str(&buf).unwrap()
    }

    fn call_tool(port: u16, id: u64, name: &str, arguments: Value) -> Value {
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        send_request(port, &req)
    }

    /// Acceptance #1: `spawn_mcp_server` binds a real loopback port.
    #[test]
    fn spawn_mcp_server_binds_loopback_port() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        assert!(server.port > 0);
        // Connect with no payload — must not hang.
        let stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        drop(stream);
        server.shutdown();
    }

    /// `tools/list` returns the 8 ESSENTIAL_TOOLS.
    #[test]
    fn tools_list_returns_eight_tools() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let resp = send_request(
            server.port,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 8, "got {:?}", tools);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for required in [
            "memory_save",
            "memory_recall",
            "memory_smart_search",
            "memory_sessions",
            "memory_consolidate",
            "memory_diagnose",
            "memory_lesson_save",
            "memory_reflect",
        ] {
            assert!(names.contains(&required), "missing {required}: {names:?}");
        }
        server.shutdown();
    }

    /// `memory_save` returns a uuidv7-shaped id.
    #[test]
    fn memory_save_returns_uuidv7_id() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let resp = call_tool(
            server.port,
            1,
            "memory_save",
            json!({ "content": "hello world" }),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        let id = body["id"].as_str().unwrap().to_string();
        let parsed = MemoryId::parse(&id).expect("uuid parse");
        // round-tripping via parse keeps it canonical, but we also want
        // to assert v7-ness directly.
        let uuid_parsed = uuid::Uuid::parse_str(parsed.as_str()).unwrap();
        assert_eq!(uuid_parsed.get_version_num(), 7);
        server.shutdown();
    }

    /// `memory_recall` returns the saved content for a known id.
    #[test]
    fn memory_recall_returns_saved_content() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let saved = call_tool(
            server.port,
            1,
            "memory_save",
            json!({ "content": "bcrypt is fine; argon2id is better" }),
        );
        let saved_text = saved["result"]["content"][0]["text"].as_str().unwrap();
        let saved_body: Value = serde_json::from_str(saved_text).unwrap();
        let id = saved_body["id"].as_str().unwrap().to_string();
        let recalled = call_tool(server.port, 2, "memory_recall", json!({ "id": id }));
        let body_text = recalled["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(body_text).unwrap();
        assert_eq!(body["id"], id);
        assert_eq!(body["content"], "bcrypt is fine; argon2id is better");
        server.shutdown();
    }

    /// `memory_smart_search` ranks the matching row first.
    #[test]
    fn memory_smart_search_returns_relevant_hit() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        call_tool(
            server.port,
            1,
            "memory_save",
            json!({ "content": "Use bcrypt for password hashing" }),
        );
        call_tool(
            server.port,
            2,
            "memory_save",
            json!({ "content": "Cats love sunny windowsills" }),
        );
        let resp = call_tool(
            server.port,
            3,
            "memory_smart_search",
            json!({ "query": "bcrypt" }),
        );
        let body_text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(body_text).unwrap();
        let results = body["results"].as_array().expect("results");
        assert!(!results.is_empty(), "expected at least one hit");
        let top = results[0]["content"].as_str().unwrap();
        assert!(
            top.contains("bcrypt"),
            "top hit should be the bcrypt row, got: {top}"
        );
        server.shutdown();
    }

    /// `memory_sessions` returns the distinct session ids we've stored.
    #[test]
    fn memory_sessions_returns_distinct_session_ids() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        call_tool(
            server.port,
            1,
            "memory_save",
            json!({ "content": "a", "session_id": "sess-A" }),
        );
        call_tool(
            server.port,
            2,
            "memory_save",
            json!({ "content": "b", "session_id": "sess-B" }),
        );
        call_tool(
            server.port,
            3,
            "memory_save",
            json!({ "content": "a2", "session_id": "sess-A" }),
        );
        let resp = call_tool(server.port, 4, "memory_sessions", json!({}));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        let mut sessions: Vec<String> = body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        sessions.sort();
        assert_eq!(sessions, vec!["sess-A".to_string(), "sess-B".to_string()]);
        server.shutdown();
    }

    /// `memory_diagnose` returns the basics: embedder name, dim, row count,
    /// schema user_version.
    #[test]
    fn memory_diagnose_returns_basics() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let resp = call_tool(server.port, 1, "memory_diagnose", json!({}));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        assert!(body["embedder"].is_string());
        assert!(body["embed_dim"].is_number());
        assert!(body["row_count"].is_number());
        assert!(body["schema_user_version"].is_number());
        server.shutdown();
    }

    /// `memory_reflect` is a documented stub in v0.1.
    #[test]
    fn memory_reflect_returns_unimplemented_error() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let resp = call_tool(server.port, 1, "memory_reflect", json!({}));
        let err = resp["error"].as_object().expect("error reply");
        let msg = err["message"].as_str().unwrap();
        assert!(
            msg.contains("v0.5") || msg.to_lowercase().contains("not yet"),
            "expected v0.5/not-yet stub message, got: {msg}"
        );
        server.shutdown();
    }

    /// `memory_lesson_save` writes into the lessons table and returns an id.
    #[test]
    fn memory_lesson_save_returns_id() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let resp = call_tool(
            server.port,
            1,
            "memory_lesson_save",
            json!({ "content": "Don't unwrap on user input." }),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        let id = body["id"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok());
        server.shutdown();
    }

    /// Invalid params should produce a JSON-RPC error response.
    #[test]
    fn memory_save_rejects_missing_content() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let resp = call_tool(server.port, 1, "memory_save", json!({}));
        let err = resp["error"].as_object().expect("error reply");
        assert_eq!(err["code"].as_i64().unwrap(), JSONRPC_INVALID_PARAMS);
        server.shutdown();
    }

    /// Parse errors propagate as JSON-RPC -32700.
    #[test]
    fn malformed_request_returns_parse_error() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        stream.write_all(b"not-json-at-all\n").unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut buf = String::new();
        reader.read_line(&mut buf).unwrap();
        let resp: Value = serde_json::from_str(&buf).unwrap();
        assert_eq!(resp["error"]["code"].as_i64().unwrap(), JSONRPC_PARSE_ERROR);
        server.shutdown();
    }

    /// JsonRpcRequest helper roundtrips its shape unchanged.
    #[test]
    fn jsonrpc_request_roundtrips() {
        let req = JsonRpcRequest::new(7, "tools/list", json!({}));
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains(r#""jsonrpc":"2.0""#));
        assert!(s.contains(r#""id":7"#));
        assert!(s.contains(r#""method":"tools/list""#));
    }

    /// `initialize` returns the MCP protocol shape Claude Code expects.
    #[test]
    fn initialize_returns_capabilities() {
        let (_tmp, _svc, server) = spawn_service_and_server();
        let resp = send_request(
            server.port,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        );
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "clud-memory");
        server.shutdown();
    }
}
