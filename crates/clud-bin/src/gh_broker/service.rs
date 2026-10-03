//! The daemon half of the read broker (#1743): TTL, single-flight,
//! conditional revalidation, write invalidation and the ledger, plus merged
//! collection reads ([`merged`]) and targeted invalidation
//! ([`super::scope`]).
//!
//! One [`GhBroker`] lives in the daemon's HTTP thread. Each `/gh/read`
//! request runs on its own thread so a slow upstream never blocks the
//! dashboard, and concurrent reads of one key share one upstream request.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::collection;
use super::scope;
use super::store::{LedgerEntry, ObjectMeta, Store};
use super::upstream::{Response, Upstream, UpstreamRequest};
use super::{ReadReply, ReadRequest};

mod merged;

/// Workflow runs and jobs change quickly while a run is live.
pub const RUNS_TTL_MS: u64 = 30_000;
pub const DEFAULT_TTL_MS: u64 = 60_000;
/// Bodies above this are served but not cached.
const MAX_CACHED_BODY: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Served from the store within its TTL, or by joining another
    /// request's upstream fetch: no upstream request of its own.
    Cache,
    /// Revalidated with `If-None-Match` / `If-Modified-Since`; `304`.
    NotModified,
    /// A merged collection brought up to date with a bounded query
    /// (`since=` / `created=>=`); the ledger records how many objects it
    /// added or changed.
    Incremental,
    Full,
    /// Upstream answered non-2xx; the shim reruns the call on the real
    /// `gh` so its error output is `gh`'s own.
    Passthrough,
    Error,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Cache => "cache",
            Outcome::NotModified => "304",
            Outcome::Incremental => "incremental",
            Outcome::Full => "full",
            Outcome::Passthrough => "passthrough",
            Outcome::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Passthrough(u16),
    Failed(String),
}

impl ReadError {
    fn outcome(&self) -> Outcome {
        match self {
            ReadError::Passthrough(_) => Outcome::Passthrough,
            ReadError::Failed(_) => Outcome::Error,
        }
    }
}

/// One answered read and what it cost, for the ledger.
struct Served {
    result: Result<Response, ReadError>,
    outcome: Outcome,
    upstream_requests: u32,
    rate: Option<u64>,
    /// Merged reads: objects added or changed by the upstream fetch.
    changed: Option<u32>,
    /// Merged reads: objects that dropped out (deleted upstream).
    removed: Option<u32>,
}

impl Served {
    fn new(
        result: Result<Response, ReadError>,
        outcome: Outcome,
        upstream_requests: u32,
        rate: Option<u64>,
    ) -> Self {
        Self {
            result,
            outcome,
            upstream_requests,
            rate,
            changed: None,
            removed: None,
        }
    }

    fn cache(response: Response) -> Self {
        Self::new(Ok(response), Outcome::Cache, 0, None)
    }

    fn failed(error: ReadError, upstream_requests: u32, rate: Option<u64>) -> Self {
        let outcome = error.outcome();
        Self::new(Err(error), outcome, upstream_requests, rate)
    }
}

/// One validated read, as the service sees it.
pub struct BrokerRead<'a> {
    pub gh: &'a Path,
    pub endpoint: &'a str,
    pub hostname: Option<&'a str>,
    pub env: &'a [(String, String)],
    pub session_id: Option<&'a str>,
    pub fresh: bool,
}

impl BrokerRead<'_> {
    fn label(&self) -> String {
        format!(
            "{}/{}",
            self.hostname.unwrap_or(""),
            self.endpoint.trim_start_matches('/')
        )
    }

    /// The store key: endpoint and host, plus a hash of the caller's
    /// identity (forwarded env and `gh` path), so two tokens never share a
    /// cached body. Secrets are hashed, never stored.
    fn key(&self) -> String {
        self.key_for(&self.label())
    }

    /// The store key of `label` under this read's identity.
    fn key_for(&self, label: &str) -> String {
        let mut identity: Vec<&(String, String)> = self.env.iter().collect();
        identity.sort();
        let mut hasher = Sha256::new();
        for (k, v) in identity {
            hasher.update(k.as_bytes());
            hasher.update([0]);
            hasher.update(v.as_bytes());
            hasher.update([0]);
        }
        hasher.update(self.gh.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(label.as_bytes());
        let digest = hasher.finalize();
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// TTL for an endpoint: runs and jobs 30 s, everything else 60 s.
pub fn ttl_ms(endpoint: &str) -> u64 {
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let live = segments
        .windows(2)
        .any(|w| w[0] == "actions" && matches!(w[1], "runs" | "jobs"));
    if live {
        RUNS_TTL_MS
    } else {
        DEFAULT_TTL_MS
    }
}

#[derive(Default)]
struct Flight {
    result: Mutex<Option<Result<Response, ReadError>>>,
    ready: Condvar,
}

impl Flight {
    fn wait(&self) -> Result<Response, ReadError> {
        let mut slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            slot = self.ready.wait(slot).unwrap_or_else(|p| p.into_inner());
        }
    }

    fn finish(&self, result: Result<Response, ReadError>) {
        let mut slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(result);
        }
        self.ready.notify_all();
    }
}

pub type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

pub struct GhBroker {
    store_path: PathBuf,
    store: Mutex<Option<Arc<Store>>>,
    upstream: Box<dyn Upstream>,
    clock: Clock,
    flights: Mutex<HashMap<String, Arc<Flight>>>,
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl GhBroker {
    /// The daemon's broker: store opened lazily on the first read, real
    /// `gh` upstream, wall clock.
    pub fn for_state_dir(state_dir: &Path) -> Self {
        Self::new(
            state_dir.join(super::STORE_FILE),
            Box::new(super::upstream::GhCli),
            Box::new(unix_ms),
        )
    }

    pub fn new(store_path: PathBuf, upstream: Box<dyn Upstream>, clock: Clock) -> Self {
        Self {
            store_path,
            store: Mutex::new(None),
            upstream,
            clock,
            flights: Mutex::new(HashMap::new()),
        }
    }

    fn store(&self) -> Result<Arc<Store>, ReadError> {
        let mut slot = self.store.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(store) = slot.as_ref() {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new(Store::open(&self.store_path).map_err(ReadError::Failed)?);
        *slot = Some(Arc::clone(&store));
        Ok(store)
    }

    pub fn ledger(&self) -> Result<Vec<LedgerEntry>, String> {
        self.store()
            .map_err(|e| format!("{e:?}"))
            .and_then(|s| s.ledger())
    }

    /// Mark every cached read stale. In-flight
    /// fetches are detached, so a read issued after the write never joins
    /// a fetch that began before it.
    pub fn invalidate(&self) -> Result<(), String> {
        let store = self.store().map_err(|e| format!("{e:?}"))?;
        store.invalidate((self.clock)())?;
        self.detach_flights();
        Ok(())
    }

    /// Mark stale only the reads that carry one of `tags`
    /// ([`scope::key_tags`]).
    pub fn invalidate_tags(&self, tags: &[String]) -> Result<(), String> {
        let store = self.store().map_err(|e| format!("{e:?}"))?;
        store.invalidate_tags(tags, (self.clock)())?;
        self.detach_flights();
        Ok(())
    }

    fn detach_flights(&self) {
        self.flights
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// The cached body for `key` if no write since its fetch could have
    /// changed it, and it is within its TTL (unless `fresh`).
    fn fresh_hit(&self, store: &Store, key: &str, endpoint: &str, fresh: bool) -> Option<Response> {
        let (meta, body) = store.get(key).ok()??;
        let stale_after = store
            .stale_after(&scope::key_tags(endpoint))
            .unwrap_or(u64::MAX);
        if meta.fetched_at_ms <= stale_after {
            return None;
        }
        let now = (self.clock)();
        let within_ttl = !fresh && now.saturating_sub(meta.fetched_at_ms) < ttl_ms(endpoint);
        within_ttl.then(|| Response {
            status: meta.status,
            headers: meta.headers,
            body: Arc::new(body),
        })
    }

    pub fn read(&self, read: &BrokerRead<'_>) -> Result<Response, ReadError> {
        let served = self.read_inner(read);
        if let Ok(store) = self.store() {
            let _ = store.append_ledger(&LedgerEntry {
                ts_ms: (self.clock)(),
                session_id: read.session_id.map(str::to_string),
                key: read.label(),
                outcome: served.outcome.as_str().to_string(),
                upstream_requests: served.upstream_requests,
                rate_remaining: served.rate,
                changed: served.changed,
                removed: served.removed,
            });
        }
        served.result
    }

    fn read_inner(&self, read: &BrokerRead<'_>) -> Served {
        let store = match self.store() {
            Ok(store) => store,
            Err(error) => return Served::failed(error, 0, None),
        };
        let mut spent = 0;
        if let Some(plan) = collection::plan(read.endpoint) {
            match self.read_collection(&store, read, &plan) {
                merged::Merged::Served(served) => return served,
                merged::Merged::Fallback(requests) => spent = requests,
            }
        }
        let mut served = self.read_object(&store, read);
        served.upstream_requests += spent;
        served
    }

    /// Join the in-flight fetch of `key`, or become its leader (`true`).
    fn join_flight(&self, key: &str) -> (Arc<Flight>, bool) {
        let mut flights = self.flights.lock().unwrap_or_else(|p| p.into_inner());
        match flights.get(key) {
            Some(flight) => (Arc::clone(flight), false),
            None => {
                let flight = Arc::new(Flight::default());
                flights.insert(key.to_string(), Arc::clone(&flight));
                (flight, true)
            }
        }
    }

    /// The phase-1 path: one object keyed by its exact URL.
    fn read_object(&self, store: &Store, read: &BrokerRead<'_>) -> Served {
        let key = read.key();
        if let Some(hit) = self.fresh_hit(store, &key, read.endpoint, read.fresh) {
            return Served::cache(hit);
        }
        let (flight, leader) = self.join_flight(&key);
        if !leader {
            return match flight.wait() {
                Ok(response) => Served::cache(response),
                Err(error) => Served::failed(error, 0, None),
            };
        }
        let landing = Landing {
            broker: self,
            key: &key,
            flight: &flight,
        };
        let served = self.fetch(store, &key, read);
        landing.finish(served.result.clone());
        served
    }

    /// The leader's work: recheck the store, then one conditional request.
    fn fetch(&self, store: &Store, key: &str, read: &BrokerRead<'_>) -> Served {
        // Another leader may have landed between our miss and our flight.
        if let Some(hit) = self.fresh_hit(store, key, read.endpoint, read.fresh) {
            return Served::cache(hit);
        }
        let cached = store.get(key).ok().flatten();
        let started = (self.clock)();
        let request = UpstreamRequest {
            gh: read.gh,
            endpoint: read.endpoint,
            hostname: read.hostname,
            env: read.env,
            if_none_match: cached.as_ref().and_then(|(m, _)| m.etag.as_deref()),
            if_modified_since: cached
                .as_ref()
                .filter(|(m, _)| m.etag.is_none())
                .and_then(|(m, _)| m.last_modified.as_deref()),
        };
        let response = match self.upstream.fetch(&request) {
            Ok(response) => response,
            Err(error) => return Served::failed(ReadError::Failed(error), 1, None),
        };
        let rate = response
            .header("x-ratelimit-remaining")
            .and_then(|v| v.parse().ok());
        if response.status == 304 {
            let Some((mut meta, body)) = cached else {
                return Served::failed(ReadError::Passthrough(304), 1, rate);
            };
            meta.fetched_at_ms = started;
            if let Some(etag) = response.header("etag") {
                meta.etag = Some(etag.to_string());
            }
            let _ = store.put(key, &meta, None);
            let served = Response {
                status: meta.status,
                headers: meta.headers,
                body: Arc::new(body),
            };
            return Served::new(Ok(served), Outcome::NotModified, 1, rate);
        }
        if !(200..300).contains(&response.status) {
            return Served::failed(ReadError::Passthrough(response.status), 1, rate);
        }
        if response.status == 200 && response.body.len() <= MAX_CACHED_BODY {
            let meta = ObjectMeta {
                label: read.label(),
                status: response.status,
                headers: response.headers.clone(),
                etag: response.header("etag").map(str::to_string),
                last_modified: response.header("last-modified").map(str::to_string),
                fetched_at_ms: started,
            };
            let _ = store.put(key, &meta, Some(&response.body));
        }
        Served::new(Ok(response), Outcome::Full, 1, rate)
    }

    /// The daemon's `/gh/read` handler body: JSON in, `(status, JSON)` out.
    /// 200 carries a [`ReadReply`]; 409 means "run the real `gh`"; 400/502
    /// are errors the shim also answers by running the real `gh`.
    pub fn handle_http(&self, body: &[u8], self_exe: &Path, home: Option<&Path>) -> (u16, Vec<u8>) {
        let request: ReadRequest = match serde_json::from_slice(body) {
            Ok(request) => request,
            Err(error) => return (400, error_json(&format!("invalid request: {error}"))),
        };
        let gh = PathBuf::from(&request.gh);
        let dirs = crate::shim_registry::shim_dirs(self_exe, None, home);
        let named_gh = crate::shim_registry::invoked_name(gh.as_os_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("gh"));
        if !named_gh || !crate::shim_registry::valid_target(&gh, self_exe, &dirs) {
            return (400, error_json("gh target is not a real gh executable"));
        }
        // The shim already classified the read; the daemon re-checks so the
        // route can only ever run `gh api -i <REST path>`.
        let hostname_ok = request
            .hostname
            .as_deref()
            .is_none_or(|h| super::classify::valid_hostname(h).is_some());
        let env_ok = request
            .env
            .iter()
            .all(|(key, _)| super::FORWARDED_ENV.contains(&key.as_str()));
        if !super::classify::brokerable_endpoint(&request.endpoint) || !hostname_ok || !env_ok {
            return (400, error_json("not a brokerable gh api read"));
        }
        let read = BrokerRead {
            gh: &gh,
            endpoint: &request.endpoint,
            hostname: request.hostname.as_deref(),
            env: &request.env,
            session_id: request.session_id.as_deref(),
            fresh: request.fresh,
        };
        match self.read(&read) {
            Ok(response) => {
                let reply = ReadReply {
                    status: response.status,
                    headers: response.headers,
                    body_b64: base64::engine::general_purpose::STANDARD.encode(&*response.body),
                    outcome: "ok".to_string(),
                };
                match serde_json::to_vec(&reply) {
                    Ok(bytes) => (200, bytes),
                    Err(error) => (502, error_json(&error.to_string())),
                }
            }
            Err(ReadError::Passthrough(status)) => {
                (409, error_json(&format!("upstream status {status}")))
            }
            Err(ReadError::Failed(error)) => (502, error_json(&error)),
        }
    }
}

/// Shim -> daemon `/gh/invalidate` body. No (or invalid) `tags` means
/// every read, the phase-1 behavior an older shim still asks for.
#[derive(serde::Deserialize)]
struct InvalidateRequest {
    #[serde(default)]
    tags: Option<Vec<String>>,
}

impl GhBroker {
    /// The daemon's `/gh/invalidate` handler body.
    pub fn handle_invalidate(&self, body: &[u8]) -> (u16, Vec<u8>) {
        let tags = serde_json::from_slice::<InvalidateRequest>(body)
            .ok()
            .and_then(|request| request.tags)
            .filter(|tags| {
                !tags.is_empty()
                    && tags.len() <= scope::MAX_TAGS
                    && tags.iter().all(|tag| scope::valid_tag(tag))
            });
        let result = match tags {
            Some(tags) => self.invalidate_tags(&tags),
            None => self.invalidate(),
        };
        match result {
            Ok(()) => (200, b"{}".to_vec()),
            Err(error) => (500, error_json(&error)),
        }
    }
}

fn error_json(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap_or_default()
}

/// Lands the leader's flight exactly once, even if the fetch panics: waiters
/// are released and the key is free for the next read.
struct Landing<'a> {
    broker: &'a GhBroker,
    key: &'a str,
    flight: &'a Arc<Flight>,
}

impl Landing<'_> {
    fn finish(self, result: Result<Response, ReadError>) {
        self.flight.finish(result);
    }
}

impl Drop for Landing<'_> {
    fn drop(&mut self) {
        self.flight
            .finish(Err(ReadError::Failed("broker fetch aborted".into())));
        let mut flights = self
            .broker
            .flights
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if flights
            .get(self.key)
            .is_some_and(|current| Arc::ptr_eq(current, self.flight))
        {
            flights.remove(self.key);
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod phase2_tests;
