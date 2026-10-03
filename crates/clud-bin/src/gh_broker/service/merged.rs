//! Merged collection reads (#1743).
//!
//! A read that [`collection::plan`] accepts is answered from the
//! collection's stored pages. Within the TTL that costs nothing. After it,
//! the broker re-sends every upstream page with the ETag it last received
//! ([`pass`]). A `304` costs no rate limit and proves the page unchanged,
//! deletions, reactions and reruns included; a `200` replaces the page. So
//! every refresh is a complete reconciliation, and a quiet collection costs
//! only free `304`s. When a pass replaced a page of a multi-page collection,
//! the pages before the last are re-checked ([`verify`]): a deletion that
//! landed between two page requests shifts the later pages, and a merge of
//! pages from both sides of it would drop an object. A collection the merge
//! cannot reproduce exactly falls back to the phase-1 exact-URL read.

use std::sync::Arc;

use super::{BrokerRead, GhBroker, Landing, Outcome, ReadError, Served};
use crate::gh_broker::collection::{self, CollectionState, Plan, StoredPage};
use crate::gh_broker::scope;
use crate::gh_broker::store::Store;
use crate::gh_broker::upstream::{Response, UpstreamRequest};

/// A collection found unmergeable takes the exact-URL path this long before
/// the merge is tried again.
pub const UNMERGEABLE_RETRY_MS: u64 = 30 * 60 * 1000;
/// A merged state larger than this is not kept.
const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;
/// The flight result that sends waiters to the exact-URL path too.
const UNMERGEABLE: &str = "collection cannot be merged";
/// Passes per refresh: the first, and one more if a change landed during it.
const MAX_PASSES: usize = 2;

pub(super) enum Merged {
    Served(Served),
    /// Use the exact-URL path; this many upstream requests were already
    /// spent finding out.
    Fallback(u32),
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
    let first = state.pages.first();
    Response {
        status: 200,
        headers: first.map(|page| page.headers.clone()).unwrap_or_default(),
        body: Arc::new(collection::render(
            plan.kind,
            &state.members(),
            first.and_then(|page| page.total_count),
            plan.per_page,
        )),
    }
}

/// One pass over the collection's pages.
enum Pass {
    /// The pages now, and whether any of them was re-sent (a `200`).
    Done(Vec<StoredPage>, bool),
    /// Pages that do not fit together (fetched across a change); they seed
    /// the next pass.
    Misfit(Vec<StoredPage>),
    Unmergeable,
}

impl GhBroker {
    fn unmergeable(&self, state: &CollectionState) -> bool {
        state
            .unmergeable_at_ms
            .is_some_and(|at| (self.clock)().saturating_sub(at) < UNMERGEABLE_RETRY_MS)
    }

    /// Within its TTL, wide enough for the caller, and no write since its
    /// last refresh names it.
    fn collection_fresh(
        &self,
        store: &Store,
        state: &CollectionState,
        tags: &[String],
        plan: &Plan,
    ) -> bool {
        let stale_after = store.stale_after(tags).unwrap_or(u64::MAX);
        state.unmergeable_at_ms.is_none()
            && state.width >= plan.width()
            && state.fetched_at_ms > stale_after
            && (self.clock)().saturating_sub(state.fetched_at_ms) < super::ttl_ms(&plan.path)
    }

    pub(super) fn read_collection(
        &self,
        store: &Store,
        read: &BrokerRead<'_>,
        plan: &Plan,
    ) -> Merged {
        let label = format!("{}/{}#pages", read.hostname.unwrap_or(""), plan.label());
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
            // from the state it stored, if that state is wide enough.
            return match flight.wait() {
                Ok(_) => match store.collection(&key).ok().flatten() {
                    Some(state) if !self.unmergeable(&state) && state.width >= plan.width() => {
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
        // A caller wider than the stored pages starts over at its width;
        // otherwise the stored width is kept, so a narrower caller never
        // shrinks what a wider one needs.
        let prior = prior.filter(|state| state.width >= plan.width());
        let width = prior.as_ref().map_or(plan.width(), |state| state.width);
        let started = (self.clock)();
        let mut fetcher = Fetcher {
            broker: self,
            read,
            requests: 0,
            rate: None,
        };
        let mut pages = prior
            .as_ref()
            .map(|state| state.pages.clone())
            .unwrap_or_default();
        // Whether any pass re-sent a page: a later pass of `304`s over pages
        // an earlier pass replaced still changed the collection.
        let mut resent_any = false;
        let mut consistent = false;
        let mut misfit = false;
        for _ in 0..MAX_PASSES {
            let step = pass(plan, &pages, width, &mut fetcher).and_then(|step| match step {
                Pass::Done(now, true) if !verify(plan, &now, width, &mut fetcher)? => {
                    Ok(Pass::Misfit(now))
                }
                other => Ok(other),
            });
            match step {
                Ok(Pass::Done(now, resent)) => {
                    pages = now;
                    resent_any |= resent;
                    consistent = true;
                    break;
                }
                Ok(Pass::Misfit(now)) => {
                    pages = now;
                    resent_any = true;
                    misfit = true;
                }
                Ok(Pass::Unmergeable) => {
                    misfit = false;
                    break;
                }
                Err(error) => {
                    return Merged::Served(Served::failed(error, fetcher.requests, fetcher.rate));
                }
            }
        }
        if misfit && !consistent {
            // The collection kept changing under both passes. This read
            // takes the exact-URL path; the stored pages stay, so the next
            // refresh revalidates them as usual.
            return Merged::Fallback(fetcher.requests);
        }
        if !consistent {
            return self.give_up(store, key, started, fetcher.requests);
        }
        let state = CollectionState {
            pages,
            width,
            fetched_at_ms: started,
            unmergeable_at_ms: None,
        };
        let size = serde_json::to_vec(&state).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_STATE_BYTES || store.put_collection(key, &state).is_err() {
            return self.give_up(store, key, started, fetcher.requests);
        }
        let mut served = Served::new(
            Ok(render(plan, &state)),
            Outcome::Full,
            fetcher.requests,
            fetcher.rate,
        );
        if let Some(prior) = &prior {
            let (changed, removed) = collection::diff(&prior.members(), &state.members());
            served.outcome = if resent_any {
                Outcome::Incremental
            } else {
                Outcome::NotModified
            };
            served.changed = Some(changed);
            served.removed = resent_any.then_some(removed);
        }
        Merged::Served(served)
    }

    /// Mark the collection unmergeable for [`UNMERGEABLE_RETRY_MS`] and send
    /// the read down the exact-URL path.
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

/// Re-send every page of the collection, each with the ETag of its stored
/// copy in `prior`. A page ends the collection only when it is short: a
/// full page that is a `304` can still be followed by a page that did not
/// exist when it was stored.
fn pass(
    plan: &Plan,
    prior: &[StoredPage],
    width: usize,
    fetcher: &mut Fetcher<'_, '_>,
) -> Result<Pass, ReadError> {
    let mut pages = Vec::new();
    let mut resent = false;
    for number in 1..=plan.max_pages() {
        let stored = prior.get(number - 1);
        let etag = stored.and_then(|page| page.etag.as_deref());
        let response = fetcher.get(&plan.seed_url(number, width), etag)?;
        let page = if response.status == 304 {
            // Only ever an answer to our own If-None-Match.
            stored.cloned().ok_or(ReadError::Passthrough(304))?
        } else {
            let Some(parsed) = collection::parse_page(plan.kind, &response.body)
                .filter(|page| collection::in_natural_order(plan.kind, &page.members))
            else {
                return Ok(Pass::Unmergeable);
            };
            let short = parsed.members.len() < width;
            if short && collection::has_next_page(response.header("link")) {
                // A short page that is not the last: not a shape the merge
                // can page through.
                return Ok(Pass::Unmergeable);
            }
            resent = true;
            StoredPage {
                etag: response.header("etag").map(str::to_string),
                members: parsed.members,
                total_count: parsed.total_count,
                headers: collection::kept_headers(&response.headers),
            }
        };
        let last = number == plan.max_pages() || page.members.len() < width;
        let full = page.members.len() >= width;
        pages.push(page);
        if last {
            if full && plan.max_pages() > 1 {
                // More pages than a merge keeps.
                return Ok(Pass::Unmergeable);
            }
            let joined: Vec<_> = pages.iter().flat_map(|p| p.members.clone()).collect();
            if !collection::in_natural_order(plan.kind, &joined) {
                return Ok(Pass::Misfit(pages));
            }
            return Ok(Pass::Done(pages, resent));
        }
    }
    Ok(Pass::Unmergeable)
}

/// After a pass re-sent a page: whether every page before the last is
/// still what the pass stored (a `304` to its new ETag). Any other answer
/// means a change landed during the pass, and the pages may not fit
/// together.
fn verify(
    plan: &Plan,
    pages: &[StoredPage],
    width: usize,
    fetcher: &mut Fetcher<'_, '_>,
) -> Result<bool, ReadError> {
    for (index, page) in pages.iter().enumerate().take(pages.len() - 1) {
        let Some(etag) = page.etag.as_deref() else {
            return Ok(false);
        };
        let response = fetcher.get(&plan.seed_url(index + 1, width), Some(etag))?;
        if response.status != 304 {
            return Ok(false);
        }
    }
    Ok(true)
}
