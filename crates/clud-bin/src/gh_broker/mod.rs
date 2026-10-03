//! The session `gh` read broker (#1743).
//!
//! In a clud session, `gh api <endpoint>` GETs are answered by the daemon
//! from a durable ETag cache instead of each call spending GitHub's shared
//! REST budget. The pieces:
//!
//! - [`classify`]: which `gh` argv the shim may broker, and which may write.
//! - [`client`]: the shim side. Asks the daemon over its loopback HTTP
//!   listener, then lets the real `gh` format the body from a one-shot local
//!   replay server, so `--jq`, `--template` and TTY output stay `gh`'s own.
//! - [`service`]: the daemon side. TTL, single-flight, conditional
//!   revalidation and the ledger, over [`store`] and the [`upstream`] `gh`.
//! - [`collection`]: merged reads of comments and run lists, kept as the
//!   upstream pages and revalidated page by page with their ETags.
//! - [`scope`]: phase 2 targeted invalidation tags for reads and writes.
//!
//! Every failure on the shim side falls back to the real `gh` unchanged.
//! See `docs/architecture/gh-read-broker.md`.

pub mod classify;
pub mod client;
pub mod collection;
pub mod scope;
pub mod service;
pub mod store;
pub mod upstream;

use serde::{Deserialize, Serialize};

/// Daemon route that answers one read.
pub const READ_PATH: &str = "/gh/read";
/// Daemon route that marks cached reads stale after a possible write: those
/// carrying the posted `tags`, or every read when there are none.
pub const INVALIDATE_PATH: &str = "/gh/invalidate";
/// Daemon route (GET) that returns the newest ledger rows, oldest first, as
/// a JSON array: what each brokered read cost upstream.
pub const LEDGER_PATH: &str = "/gh/ledger";
/// Rows the ledger route returns at most.
pub const LEDGER_ROWS: usize = 1000;
/// The broker's store, under the daemon state dir.
pub const STORE_FILE: &str = "gh-broker.redb";

/// Environment the upstream `gh` needs to authenticate and route exactly as
/// the caller's `gh` would. The shim forwards these values; the daemon sets
/// them on the upstream child and removes any it was not sent. Their hash is
/// part of the cache key, so two identities never share a cached body. No
/// value is ever stored or logged.
pub const FORWARDED_ENV: &[&str] = &[
    "GH_HOST",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// Shim -> daemon: one `gh api` read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRequest {
    /// The session's real `gh` (`CLUD_GH_SHIM_TARGET`); the daemon
    /// revalidates it before running it.
    pub gh: String,
    pub endpoint: String,
    pub hostname: Option<String>,
    /// [`FORWARDED_ENV`] values the caller had set.
    pub env: Vec<(String, String)>,
    pub session_id: Option<String>,
    /// `CLUD_GH_FRESH=1`: skip the TTL, still revalidate conditionally.
    #[serde(default)]
    pub fresh: bool,
}

/// Daemon -> shim: the response to replay to the real `gh`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_b64: String,
    pub outcome: String,
}
