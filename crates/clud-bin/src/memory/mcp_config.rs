//! Issue #265: idempotent registration of the `clud-memory` MCP server and the
//! `clud hook` entries into the user's Claude Code and Codex config files.
//!
//! Four files are written, all under `$HOME`:
//!
//! - `~/.claude.json`         (JSON)  — `mcpServers."clud-memory"` block.
//! - `~/.codex/config.toml`   (TOML)  — `[mcp_servers.clud-memory]` table.
//! - `~/.claude/settings.json`(JSON)  — four `hooks.<Event>[]` entries.
//! - `~/.codex/hooks.json`    (JSON)  — same four entries (Codex hook file is
//!   JSON; see `codex_hook_normalize.rs`).
//!
//! Idempotency: a managed block carries a marker (`_clud_managed: true` in
//! JSON, a `# managed-by: clud-memory` lead comment in TOML). Re-running
//! against an already-up-to-date file is a no-op.
//!
//! Refuse-to-clobber: when a `clud-memory` MCP key or a `clud hook ...`
//! command exists *without* the managed marker, the helper returns
//! `Error::UserDefined { .. }` so the caller can warn and skip. The user can
//! rename their entry, hand-edit the marker in, or re-run `clud --setup` to
//! reapply.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;
use serde_json::{json, Value};
use toml_edit::{value, DocumentMut, Item, Table};

const MANAGED_MARKER: &str = "_clud_managed";
const VERSION_MARKER: &str = "_clud_version";
const MCP_SERVER_KEY: &str = "clud-memory";
const TOML_MANAGED_COMMENT: &str = "managed-by: clud-memory";

const HOOK_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "clud hook session-start"),
    ("UserPromptSubmit", "clud hook user-prompt-submit"),
    ("PostToolUse", "clud hook post-tool-use"),
    ("Stop", "clud hook stop"),
];

const HOOK_TIMEOUT_SECS: u64 = 30;

/// Outcome of a single ensure_* / remove_* call. The `Wrote` / `AlreadyPresent`
/// split lets the caller stay quiet on idempotent re-runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Wrote,
    AlreadyPresent,
    Removed,
    NotPresent,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    Toml(toml_edit::TomlError),
    /// The target key exists in the config without our managed marker. The
    /// caller must surface a warning and skip the write.
    UserDefined {
        path: PathBuf,
        key: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(error) => write!(f, "{error}"),
            Error::Json(error) => write!(f, "{error}"),
            Error::Toml(error) => write!(f, "{error}"),
            Error::UserDefined { path, key } => write!(
                f,
                "refusing to overwrite user-defined `{key}` in {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::Json(error)
    }
}

impl From<toml_edit::TomlError> for Error {
    fn from(error: toml_edit::TomlError) -> Self {
        Error::Toml(error)
    }
}

fn claude_json_path(home: &Path) -> PathBuf {
    home.join(".claude.json")
}

fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn codex_hooks_path(home: &Path) -> PathBuf {
    home.join(".codex").join("hooks.json")
}

fn lock_path(home: &Path, name: &str) -> PathBuf {
    home.join(".clud").join(name)
}

/// `true` iff the `clud-memory` MCP block is registered in either the Claude
/// or the Codex config. Used by the scope selector to choose its default row.
pub fn memory_already_registered(home: &Path) -> bool {
    if let Ok(text) = std::fs::read_to_string(claude_json_path(home)) {
        if let Ok(json) = serde_json::from_str::<Value>(&text) {
            if let Some(block) = json
                .get("mcpServers")
                .and_then(|v| v.get(MCP_SERVER_KEY))
                .and_then(|v| v.as_object())
            {
                if block
                    .get(MANAGED_MARKER)
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(codex_config_path(home)) {
        if let Ok(doc) = text.parse::<DocumentMut>() {
            if doc
                .get("mcp_servers")
                .and_then(|i| i.as_table())
                .and_then(|t| t.get(MCP_SERVER_KEY))
                .is_some()
            {
                let header_managed = doc
                    .get("mcp_servers")
                    .and_then(|i| i.as_table())
                    .and_then(|t| t.get(MCP_SERVER_KEY))
                    .map(|item| {
                        item.as_table()
                            .map(|t| {
                                let decor = t.decor();
                                let prefix = decor.prefix().and_then(|s| s.as_str()).unwrap_or("");
                                prefix.contains(TOML_MANAGED_COMMENT)
                            })
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if header_managed {
                    return true;
                }
            }
        }
    }
    false
}

// ---------- Atomic-write + lock helpers ---------------------------------

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

struct LockGuard {
    _file: File,
}

fn acquire_lock(path: &Path) -> io::Result<LockGuard> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    FileExt::lock_exclusive(&file)
        .map_err(|e| io::Error::other(format!("lock_exclusive {}: {e}", path.display())))?;
    Ok(LockGuard { _file: file })
}

// ---------- Managed-block builders --------------------------------------

fn managed_mcp_block(clud_version: &str) -> Value {
    json!({
        "command": "clud",
        "args": ["mcp"],
        MANAGED_MARKER: true,
        VERSION_MARKER: clud_version,
    })
}

fn mcp_block_matches(existing: &Value, clud_version: &str) -> bool {
    existing
        .get("command")
        .and_then(|v| v.as_str())
        .map(|s| s == "clud")
        .unwrap_or(false)
        && existing
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| arr.len() == 1 && arr[0].as_str() == Some("mcp"))
            .unwrap_or(false)
        && existing
            .get(VERSION_MARKER)
            .and_then(|v| v.as_str())
            .map(|s| s == clud_version)
            .unwrap_or(false)
}

fn hook_entry(command: &str) -> Value {
    json!({
        "hooks": [
            {
                "type": "command",
                "command": command,
                "timeout": HOOK_TIMEOUT_SECS,
                MANAGED_MARKER: true,
            }
        ]
    })
}

fn hook_array_is_user_defined(arr: &[Value], command: &str) -> bool {
    arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|inner| {
                inner.iter().any(|h| {
                    h.get("command").and_then(|v| v.as_str()) == Some(command)
                        && h.get(MANAGED_MARKER).and_then(|v| v.as_bool()) != Some(true)
                })
            })
            .unwrap_or(false)
    })
}

fn find_managed_index(arr: &[Value], command: &str) -> Option<usize> {
    arr.iter().position(|entry| {
        entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|inner| {
                inner.iter().any(|h| {
                    h.get("command").and_then(|v| v.as_str()) == Some(command)
                        && h.get(MANAGED_MARKER).and_then(|v| v.as_bool()) == Some(true)
                })
            })
            .unwrap_or(false)
    })
}

// ---------- Claude MCP --------------------------------------------------

pub fn ensure_claude_mcp(
    home: &Path,
    clud_version: &str,
    out: &mut dyn Write,
) -> Result<Outcome, Error> {
    let path = claude_json_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-claude-mcp.lock"))?;
    let outcome = write_claude_mcp(&path, clud_version)?;
    log_outcome(out, &path, "mcpServers.clud-memory", outcome);
    Ok(outcome)
}

fn read_claude_json(path: &Path) -> Result<Value, Error> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(&text)?)
}

fn write_claude_mcp(path: &Path, clud_version: &str) -> Result<Outcome, Error> {
    let mut json = read_claude_json(path)?;
    if !json.is_object() {
        json = json!({});
    }
    let root = json.as_object_mut().expect("root is object");
    let servers = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    let servers = servers.as_object_mut().expect("servers is object");
    if let Some(existing) = servers.get(MCP_SERVER_KEY) {
        let managed = existing
            .get(MANAGED_MARKER)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !managed {
            return Err(Error::UserDefined {
                path: path.to_path_buf(),
                key: format!("mcpServers.{MCP_SERVER_KEY}"),
            });
        }
        if mcp_block_matches(existing, clud_version) {
            return Ok(Outcome::AlreadyPresent);
        }
    }
    servers.insert(MCP_SERVER_KEY.to_string(), managed_mcp_block(clud_version));
    let mut serialized = serde_json::to_string_pretty(&json)?;
    if !serialized.ends_with('\n') {
        serialized.push('\n');
    }
    atomic_write(path, serialized.as_bytes())?;
    Ok(Outcome::Wrote)
}

pub fn remove_claude_mcp(home: &Path) -> Result<Outcome, Error> {
    let path = claude_json_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-claude-mcp.lock"))?;
    if !path.exists() {
        return Ok(Outcome::NotPresent);
    }
    let mut json = read_claude_json(&path)?;
    let Some(root) = json.as_object_mut() else {
        return Ok(Outcome::NotPresent);
    };
    let Some(servers) = root.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return Ok(Outcome::NotPresent);
    };
    let Some(existing) = servers.get(MCP_SERVER_KEY) else {
        return Ok(Outcome::NotPresent);
    };
    let managed = existing
        .get(MANAGED_MARKER)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !managed {
        return Err(Error::UserDefined {
            path,
            key: format!("mcpServers.{MCP_SERVER_KEY}"),
        });
    }
    servers.remove(MCP_SERVER_KEY);
    let mut serialized = serde_json::to_string_pretty(&json)?;
    if !serialized.ends_with('\n') {
        serialized.push('\n');
    }
    atomic_write(&path, serialized.as_bytes())?;
    Ok(Outcome::Removed)
}

// ---------- Codex MCP (TOML) -------------------------------------------

pub fn ensure_codex_mcp(
    home: &Path,
    clud_version: &str,
    out: &mut dyn Write,
) -> Result<Outcome, Error> {
    let path = codex_config_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-codex-mcp.lock"))?;
    let outcome = write_codex_mcp(&path, clud_version)?;
    log_outcome(out, &path, "mcp_servers.clud-memory", outcome);
    Ok(outcome)
}

fn read_codex_toml(path: &Path) -> Result<DocumentMut, Error> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    Ok(text.parse::<DocumentMut>()?)
}

fn codex_block_table_is_managed(table: &Table) -> bool {
    let decor = table.decor();
    let prefix = decor.prefix().and_then(|s| s.as_str()).unwrap_or("");
    prefix.contains(TOML_MANAGED_COMMENT)
}

fn codex_block_matches(table: &Table, clud_version: &str) -> bool {
    let cmd_ok = table
        .get("command")
        .and_then(|i| i.as_str())
        .map(|s| s == "clud")
        .unwrap_or(false);
    let args_ok = table
        .get("args")
        .and_then(|i| i.as_array())
        .map(|arr| {
            arr.len() == 1
                && arr
                    .get(0)
                    .and_then(|v| v.as_str())
                    .map(|s| s == "mcp")
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    let version_ok = table
        .get(VERSION_MARKER)
        .and_then(|i| i.as_str())
        .map(|s| s == clud_version)
        .unwrap_or(false);
    cmd_ok && args_ok && version_ok
}

fn build_codex_mcp_table(clud_version: &str) -> Table {
    let mut t = Table::new();
    t.set_implicit(false);
    t.insert("command", value("clud"));
    let mut args = toml_edit::Array::new();
    args.push("mcp");
    t.insert("args", value(args));
    t.insert(VERSION_MARKER, value(clud_version));
    let decor = t.decor_mut();
    decor.set_prefix(format!("\n# {TOML_MANAGED_COMMENT}\n"));
    t
}

fn write_codex_mcp(path: &Path, clud_version: &str) -> Result<Outcome, Error> {
    let mut doc = read_codex_toml(path)?;
    let servers_item = doc
        .entry("mcp_servers")
        .or_insert_with(|| Item::Table(Table::new()));
    if !servers_item.is_table() {
        *servers_item = Item::Table(Table::new());
    }
    let servers = servers_item.as_table_mut().expect("table");
    servers.set_implicit(true);

    if let Some(existing) = servers.get(MCP_SERVER_KEY) {
        let existing_table = match existing.as_table() {
            Some(t) => t,
            None => {
                return Err(Error::UserDefined {
                    path: path.to_path_buf(),
                    key: format!("mcp_servers.{MCP_SERVER_KEY}"),
                });
            }
        };
        if !codex_block_table_is_managed(existing_table) {
            return Err(Error::UserDefined {
                path: path.to_path_buf(),
                key: format!("mcp_servers.{MCP_SERVER_KEY}"),
            });
        }
        if codex_block_matches(existing_table, clud_version) {
            return Ok(Outcome::AlreadyPresent);
        }
    }
    servers.insert(
        MCP_SERVER_KEY,
        Item::Table(build_codex_mcp_table(clud_version)),
    );
    atomic_write(path, doc.to_string().as_bytes())?;
    Ok(Outcome::Wrote)
}

pub fn remove_codex_mcp(home: &Path) -> Result<Outcome, Error> {
    let path = codex_config_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-codex-mcp.lock"))?;
    if !path.exists() {
        return Ok(Outcome::NotPresent);
    }
    let mut doc = read_codex_toml(&path)?;
    let Some(servers) = doc.get_mut("mcp_servers").and_then(|i| i.as_table_mut()) else {
        return Ok(Outcome::NotPresent);
    };
    let managed = servers
        .get(MCP_SERVER_KEY)
        .and_then(|i| i.as_table())
        .map(codex_block_table_is_managed)
        .unwrap_or(false);
    if servers.get(MCP_SERVER_KEY).is_none() {
        return Ok(Outcome::NotPresent);
    }
    if !managed {
        return Err(Error::UserDefined {
            path,
            key: format!("mcp_servers.{MCP_SERVER_KEY}"),
        });
    }
    servers.remove(MCP_SERVER_KEY);
    atomic_write(&path, doc.to_string().as_bytes())?;
    Ok(Outcome::Removed)
}

// ---------- Claude hooks -------------------------------------------------

pub fn ensure_claude_hooks(
    home: &Path,
    _clud_version: &str,
    out: &mut dyn Write,
) -> Result<Outcome, Error> {
    let path = claude_settings_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-claude-hooks.lock"))?;
    let outcome = write_hooks_json(&path)?;
    log_outcome(
        out,
        &path,
        "hooks.{SessionStart,UserPromptSubmit,PostToolUse,Stop}",
        outcome,
    );
    Ok(outcome)
}

pub fn ensure_codex_hooks(
    home: &Path,
    _clud_version: &str,
    out: &mut dyn Write,
) -> Result<Outcome, Error> {
    let path = codex_hooks_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-codex-hooks.lock"))?;
    let outcome = write_hooks_json(&path)?;
    log_outcome(
        out,
        &path,
        "hooks.{SessionStart,UserPromptSubmit,PostToolUse,Stop}",
        outcome,
    );
    Ok(outcome)
}

fn read_hooks_json(path: &Path) -> Result<Value, Error> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(&text)?)
}

fn write_hooks_json(path: &Path) -> Result<Outcome, Error> {
    let mut json = read_hooks_json(path)?;
    if !json.is_object() {
        json = json!({});
    }
    let root = json.as_object_mut().expect("root is object");
    let hooks = root.entry("hooks".to_string()).or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().expect("hooks is object");

    let mut wrote_anything = false;
    let mut already_all = true;

    for (event, command) in HOOK_EVENTS {
        let arr_item = hooks
            .entry((*event).to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !arr_item.is_array() {
            *arr_item = Value::Array(Vec::new());
        }
        let arr = arr_item.as_array_mut().expect("array");
        if hook_array_is_user_defined(arr, command) {
            return Err(Error::UserDefined {
                path: path.to_path_buf(),
                key: format!("hooks.{event}[].hooks[]"),
            });
        }
        let want = hook_entry(command);
        match find_managed_index(arr, command) {
            Some(idx) => {
                if arr[idx] != want {
                    arr[idx] = want;
                    wrote_anything = true;
                    already_all = false;
                }
            }
            None => {
                arr.push(want);
                wrote_anything = true;
                already_all = false;
            }
        }
    }

    if !wrote_anything && already_all {
        return Ok(Outcome::AlreadyPresent);
    }
    let mut serialized = serde_json::to_string_pretty(&json)?;
    if !serialized.ends_with('\n') {
        serialized.push('\n');
    }
    atomic_write(path, serialized.as_bytes())?;
    Ok(Outcome::Wrote)
}

pub fn remove_claude_hooks(home: &Path) -> Result<Outcome, Error> {
    let path = claude_settings_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-claude-hooks.lock"))?;
    remove_hooks_json(&path)
}

pub fn remove_codex_hooks(home: &Path) -> Result<Outcome, Error> {
    let path = codex_hooks_path(home);
    let _lock = acquire_lock(&lock_path(home, "memory-codex-hooks.lock"))?;
    remove_hooks_json(&path)
}

fn remove_hooks_json(path: &Path) -> Result<Outcome, Error> {
    if !path.exists() {
        return Ok(Outcome::NotPresent);
    }
    let mut json = read_hooks_json(path)?;
    let Some(root) = json.as_object_mut() else {
        return Ok(Outcome::NotPresent);
    };
    let Some(hooks) = root.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return Ok(Outcome::NotPresent);
    };

    let mut removed_any = false;
    for (event, command) in HOOK_EVENTS {
        let Some(arr_item) = hooks.get_mut(*event) else {
            continue;
        };
        let Some(arr) = arr_item.as_array_mut() else {
            continue;
        };
        let before = arr.len();
        arr.retain(|entry| {
            entry
                .get("hooks")
                .and_then(|v| v.as_array())
                .map(|inner| {
                    !inner.iter().any(|h| {
                        h.get("command").and_then(|v| v.as_str()) == Some(command)
                            && h.get(MANAGED_MARKER).and_then(|v| v.as_bool()) == Some(true)
                    })
                })
                .unwrap_or(true)
        });
        if arr.len() != before {
            removed_any = true;
        }
    }
    if !removed_any {
        return Ok(Outcome::NotPresent);
    }
    let mut serialized = serde_json::to_string_pretty(&json)?;
    if !serialized.ends_with('\n') {
        serialized.push('\n');
    }
    atomic_write(path, serialized.as_bytes())?;
    Ok(Outcome::Removed)
}

// ---------- Dry-run preview ---------------------------------------------

/// Print a summary of what the four ensure_* helpers would do for `home`
/// without actually writing. Caller is expected to gate this behind a
/// `--dry-run` flag.
pub fn print_dry_run_summary(home: &Path, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "[clud setup] would write to {}:",
        claude_json_path(home).display()
    )?;
    writeln!(out, "  + mcpServers.clud-memory.{{command, args}}")?;
    writeln!(
        out,
        "[clud setup] would write to {}:",
        codex_config_path(home).display()
    )?;
    writeln!(out, "  + [mcp_servers.clud-memory] {{command, args}}")?;
    writeln!(
        out,
        "[clud setup] would write to {}:",
        claude_settings_path(home).display()
    )?;
    writeln!(out, "  + hooks.SessionStart[].hooks[]")?;
    writeln!(out, "  + hooks.UserPromptSubmit[].hooks[]")?;
    writeln!(out, "  + hooks.PostToolUse[].hooks[]")?;
    writeln!(out, "  + hooks.Stop[].hooks[]")?;
    writeln!(
        out,
        "[clud setup] would write to {}:",
        codex_hooks_path(home).display()
    )?;
    writeln!(out, "  + hooks.SessionStart[].hooks[]")?;
    writeln!(out, "  + hooks.UserPromptSubmit[].hooks[]")?;
    writeln!(out, "  + hooks.PostToolUse[].hooks[]")?;
    writeln!(out, "  + hooks.Stop[].hooks[]")?;
    out.flush()
}

fn log_outcome(out: &mut dyn Write, path: &Path, key: &str, outcome: Outcome) {
    match outcome {
        Outcome::Wrote => {
            let _ = writeln!(
                out,
                "[clud] registered {key} in {} (clud-memory)",
                path.display()
            );
        }
        Outcome::Removed => {
            let _ = writeln!(out, "[clud] removed {key} from {}", path.display());
        }
        Outcome::AlreadyPresent | Outcome::NotPresent => {}
    }
}

#[cfg(test)]
#[path = "mcp_config_tests.rs"]
mod tests;
