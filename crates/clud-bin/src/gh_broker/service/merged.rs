//! Merged collection reads (#1743, phase 2).
//!
//! A read that [`collection::plan`] accepts is answered from the
//! collection's stored membership. Within the TTL that costs nothing. After
//! it, the broker runs the narrowest query that brings the membership up to
//! date ([`collection::Plan::delta_url`]), upserts the result, and renders
//! the caller's page. A full fetch (the seed) happens on the first read,
//! after a write that names the collection, and at most every
//! [`RECONCILE_MS`] otherwise, so a deleted object, which `since=` cannot
//! see, drops out. A collection the merge cannot reproduce exactly falls
//! back to the phase-1 exact-URL read.

use std::sync::Arc;

use super::{BrokerRead, GhBroker, Landing, Outcome, ReadError, Served};
use crate::gh_broker::collection::{self, CollectionState, Kind, Member, Plan};
use crate::gh_broker::scope;
use crate::gh_broker::store::Store;
use crate::gh_broker::upstream::{Response, UpstreamRequest};

/// A live collection is re-fetched in full at most this often.
pub const RECONCILE_MS: u64 = 30 * 60 * 1000;
/// A merged state larger than this is not kept.
const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;
/// The flight result that sends waiters to the exact-URL path too.
const UNMERGEABLE: &str = "collection cannot be merged";

pub(super) enum Merged {
    Served(Served),
    /// Use the exact-URL path; this many upstream requests were already
    /// spent finding out.
    Fallback(u32),
}

enum Refresh {
    Done(Box<CollectionState>, Outcome, Option<u32>),
    /// A delta too large to page through: fetch the collection in full.
    Reseed,
    Unmergeable,
}

/// Counts and rate-tracks the upstream requests of one refresh.
struct Fetcher<'a, 'b> {
    broker: &'a GhBroker,
    read: &'a BrokerRead<'b>,
    requests: u32,
    rate: Option<u64>,
}

impl Fetcher<'_, '_> {
    /// One upstream GET. `Ok` for a 2xx or a `304`; any other status is
    /// the caller's error, which the shim answers with the real `gh`.
    fn get(&mut self, endpoint: &str, etag: Option<&str>) -> Result<Response, ReadError> {
        self.requests += 1;
        let response = self
            .broker
            .upstream
            .fetch(&UpstreamRequest {
                gh: self.read.gh,
                endpoint,
                hostname: self.read.hostname,
                env: self.read.env,
                if_none_match: etag,
                if_modified_since: None,
            })
            .map_err(ReadError::Failed)?;
        if let Some(rate) = response
            .header("x-ratelimit-remaining")
            .and_then(|v| v.parse().ok())
        {
            self.rate = Some(rate);
        }
        if response.status != 304 && !(200..300).contains(&response.status) {
            return Err(ReadError::Passthrough(response.status));
        }
        Ok(response)
    }
}

/// The caller's page of `state`.
fn render(plan: &Plan, state: &CollectionState) -> Response {
    Response {
        status: 200,
        headers: state.headers.clone(),
        body: Arc::new(collection::render(
            plan.kind,
            &state.members,
            state.total_count,
            plan.per_page,
        )),
    }
}

fn is_live(member: &Member) -> bool {
    member
        .status
        .as_deref()
        .is_some_and(|status| status != "completed")
}

impl GhBroker {
    fn unmergeable(&self, state: &CollectionState) -> bool {
        state
            .unmergeable_at_ms
            .is_some_and(|at| (self.clock)().saturating_sub(at) < RECONCILE_MS)
    }

    /// Within its TTL, and no write since its last refresh names it.
    fn collection_fresh(
        &self,
        store: &Store,
        state: &CollectionState,
        tags: &[String],
        plan: &Plan,
    ) -> bool {
        let stale_after = store.stale_after(tags).unwrap_or(u64::MAX);
        state.unmergeable_at_ms.is_none()
            && state.fetched_at_ms > stale_after
            && (self.clock)().saturating_sub(state.fetched_at_ms) < super::ttl_ms(&plan.path)
    }

    pub(super) fn read_collection(
        &self,
        store: &Store,
        read: &BrokerRead<'_>,
        plan: &Plan,
    ) -> Merged {
        let label = format!("{}/{}#merged", read.hostname.unwrap_or(""), plan.label());
        let key = read.key_for(&label);
        let tags = scope::key_tags(&plan.path);
        if let Some(state) = store.collection(&key).ok().flatten() {
            if self.unmergeable(&state) {
                return Merged::Fallback(0);
            }
            if !read.fresh && self.collection_fresh(store, &state, &tags, plan) {
                return Merged::Served(Served::cache(render(plan, &state)));
            }
        }
        let (flight, leader) = self.join_flight(&key);
        if !leader {
            // The leader may have answered another page size: render ours
            // from the state it stored.
            return match flight.wait() {
                Ok(_) => match store.collection(&key).ok().flatten() {
                    Some(state) if !self.unmergeable(&state) => {
                        Merged::Served(Served::cache(render(plan, &state)))
                    }
                    _ => Merged::Fallback(0),
                },
                Err(ReadError::Failed(message)) if message == UNMERGEABLE => Merged::Fallback(0),
                Err(error) => Merged::Served(Served::failed(error, 0, None)),
            };
        }
        let landing = Landing {
            broker: self,
            key: &key,
            flight: &flight,
        };
        let merged = self.refresh_collection(store, read, plan, &key, &tags);
        landing.finish(match &merged {
            Merged::Served(served) => served.result.clone(),
            Merged::Fallback(_) => Err(ReadError::Failed(UNMERGEABLE.into())),
        });
        merged
    }

    fn refresh_collection(
        &self,
        store: &Store,
        read: &BrokerRead<'_>,
        plan: &Plan,
        key: &str,
        tags: &[String],
    ) -> Merged {
        let prior = store
            .collection(key)
            .ok()
            .flatten()
            .filter(|state| state.unmergeable_at_ms.is_none());
        if let Some(state) = &prior {
            // Another leader may have landed between our miss and our flight.
            if !read.fresh && self.collection_fresh(store, state, tags, plan) {
                return Merged::Served(Served::cache(render(plan, state)));
            }
        }
        let started = (self.clock)();
        let mut fetcher = Fetcher {
            broker: self,
            read,
            requests: 0,
            rate: None,
        };
        // A write that named this collection may have deleted from it, which
        // `since=` cannot see; a single-page seed revalidates for free.
        let stale_after = store.stale_after(tags).unwrap_or(u64::MAX);
        let step = match &prior {
            Some(state)
                if state.fetched_at_ms > stale_after
                    && started.saturating_sub(state.reconciled_at_ms) < RECONCILE_MS =>
            {
                match delta(plan, state, &mut fetcher, started) {
                    Ok(Refresh::Reseed) => seed(plan, prior.as_ref(), &mut fetcher, started),
                    other => other,
                }
            }
            _ => seed(plan, prior.as_ref(), &mut fetcher, started),
        };
        let (state, outcome, changed) = match step {
            Ok(Refresh::Done(state, outcome, changed)) => (*state, outcome, changed),
            Ok(Refresh::Reseed | Refresh::Unmergeable) => {
                return self.give_up(store, key, started, fetcher.requests);
            }
            Err(error) => {
                return Merged::Served(Served::failed(error, fetcher.requests, fetcher.rate));
            }
        };
        let size = serde_json::to_vec(&state).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_STATE_BYTES || store.put_collection(key, &state).is_err() {
            return self.give_up(store, key, started, fetcher.requests);
        }
        let mut served = Served::new(
            Ok(render(plan, &state)),
            outcome,
            fetcher.requests,
            fetcher.rate,
        );
        served.changed = changed;
        Merged::Served(served)
    }

    /// Mark the collection unmergeable for a reconciliation interval and
    /// send the read down the exact-URL path.
    fn give_up(&self, store: &Store, key: &str, started: u64, spent: u32) -> Merged {
        let marker = CollectionState {
            unmergeable_at_ms: Some(started),
            fetched_at_ms: started,
            ..CollectionState::default()
        };
        let _ = store.put_collection(key, &marker);
        Merged::Fallback(spent)
    }
}

fn finish(kind: Kind, mut members: Vec<Member>, prior_max_id: u64) -> (Vec<Member>, u64) {
    let max_id = members
        .iter()
        .map(|m| m.id)
        .max()
        .unwrap_or(0)
        .max(prior_max_id);
    if kind == Kind::Runs {
        members.truncate(collection::MAX_RUN_MEMBERS);
    }
    (members, max_id)
}

/// Fetch the whole collection (a run list: its newest page). With a prior
/// single-page seed's ETag, an unchanged collection is a free `304`.
fn seed(
    plan: &Plan,
    prior: Option<&CollectionState>,
    fetcher: &mut Fetcher<'_, '_>,
    started: u64,
) -> Result<Refresh, ReadError> {
    let etag = prior.and_then(|state| state.seed_etag.as_deref());
    let mut members = Vec::new();
    let mut total_count = None;
    let mut headers = Vec::new();
    let mut seed_etag = None;
    for page in 1..=collection::MAX_SEED_PAGES {
        let response = fetcher.get(&plan.seed_url(page), etag.filter(|_| page == 1))?;
        if response.status == 304 {
            let (Some(prior), 1) = (prior, page) else {
                return Err(ReadError::Passthrough(304));
            };
            let mut state = prior.clone();
            state.fetched_at_ms = started;
            state.reconciled_at_ms = started;
            return Ok(Refresh::Done(
                Box::new(state),
                Outcome::NotModified,
                Some(0),
            ));
        }
        let Some(parsed) = collection::parse_page(plan.kind, &response.body) else {
            return Ok(Refresh::Unmergeable);
        };
        let more = plan.seeds_all_pages() && collection::has_next_page(response.header("link"));
        if page == 1 {
            // An ETag covers one page; a multi-page seed re-pages in full.
            seed_etag = (!more)
                .then(|| response.header("etag").map(str::to_string))
                .flatten();
            total_count = parsed.total_count;
            headers = collection::kept_headers(&response.headers);
        }
        members.extend(parsed.members);
        if !more {
            let mut merged = Vec::new();
            collection::upsert(plan.kind, &mut merged, members);
            let (members, max_id) =
                finish(plan.kind, merged, prior.map_or(0, |state| state.max_id));
            let state = CollectionState {
                members,
                total_count,
                max_id,
                headers,
                seed_etag,
                delta_url: None,
                delta_etag: None,
                fetched_at_ms: started,
                reconciled_at_ms: started,
                unmergeable_at_ms: None,
            };
            return Ok(Refresh::Done(Box::new(state), Outcome::Full, None));
        }
    }
    Ok(Refresh::Unmergeable)
}

/// Fetch only what changed at or after the high-water mark (minus the
/// overlap), page through it, and upsert it into `prior`.
fn delta(
    plan: &Plan,
    prior: &CollectionState,
    fetcher: &mut Fetcher<'_, '_>,
    started: u64,
) -> Result<Refresh, ReadError> {
    let Some(bound) = collection::delta_bound(plan.kind, &prior.members, is_live) else {
        return Ok(Refresh::Reseed);
    };
    let mut incoming = Vec::new();
    let mut first: Option<(String, Option<String>, Vec<(String, String)>)> = None;
    for page in 1..=collection::MAX_DELTA_PAGES {
        let url = plan.delta_url(&bound, page);
        // An unchanged bound repeats the last URL: revalidate it.
        let etag = prior
            .delta_etag
            .as_deref()
            .filter(|_| page == 1 && prior.delta_url.as_deref() == Some(url.as_str()));
        let response = fetcher.get(&url, etag)?;
        if response.status == 304 {
            if etag.is_none() {
                return Err(ReadError::Passthrough(304));
            }
            let mut state = prior.clone();
            state.fetched_at_ms = started;
            return Ok(Refresh::Done(
                Box::new(state),
                Outcome::NotModified,
                Some(0),
            ));
        }
        let Some(parsed) = collection::parse_page(plan.kind, &response.body) else {
            return Ok(Refresh::Unmergeable);
        };
        let more = collection::has_next_page(response.header("link"));
        if page == 1 {
            let etag = (!more)
                .then(|| response.header("etag").map(str::to_string))
                .flatten();
            first = Some((url, etag, collection::kept_headers(&response.headers)));
        }
        incoming.extend(parsed.members);
        if more {
            continue;
        }
        let mut members = prior.members.clone();
        let (changed, added) = collection::upsert(plan.kind, &mut members, incoming);
        let mut state = prior.clone();
        if plan.kind == Kind::Runs {
            // Run ids grow with creation: a higher id than any seen is a new
            // run, which GitHub's `total_count` also counts.
            let new_runs = added.iter().filter(|id| **id > prior.max_id).count() as u64;
            state.total_count = Some(prior.total_count.unwrap_or(0) + new_runs);
        }
        (state.members, state.max_id) = finish(plan.kind, members, prior.max_id);
        if changed > 0 {
            // The seed's ETag describes the collection before this change.
            // Sending it later could `304` once a merged object is deleted
            // again and bring that object back.
            state.seed_etag = None;
        }
        if let Some((url, etag, headers)) = first {
            state.delta_url = Some(url);
            state.delta_etag = etag;
            state.headers = headers;
        }
        state.fetched_at_ms = started;
        return Ok(Refresh::Done(
            Box::new(state),
            Outcome::Incremental,
            Some(changed),
        ));
    }
    Ok(Refresh::Reseed)
}
