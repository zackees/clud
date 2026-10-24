//! `POST /proc/reap` — the dashboard's "reap subtree" action (#340).
//!
//! The body names a process by full identity, `{pid, start_time}`, copied
//! from a Processes-tab row. The daemon acts only when that identity is a
//! live row of its own cached sample, so the route cannot reach a process
//! clud is not already tracking, and a PID recycled since the page rendered
//! fails the match. The kill itself goes through
//! [`crate::process_tree::kill_tree_filtered_automatic`], which re-checks
//! every target's identity at kill time and prunes declared daemons.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use tiny_http::Request;

use super::super::types::ProcTreeSnapshot;
use super::{json_error_bytes, read_body, respond_json};
use crate::process_identity::{ProcessIdentity, UNKNOWN_START_TIME};

#[derive(Debug, Deserialize)]
struct ReapRequest {
    pid: u32,
    start_time: u64,
}

/// Pure: may the dashboard reap `identity` given the daemon's sample?
pub(super) fn authorize(
    snapshot: &ProcTreeSnapshot,
    identity: ProcessIdentity,
) -> Result<(), &'static str> {
    if identity.start_time == UNKNOWN_START_TIME {
        return Err("start_time required");
    }
    if identity.pid == snapshot.sampler_pid || identity.pid == std::process::id() {
        return Err("refusing to reap the daemon");
    }
    let tracked = snapshot
        .rows
        .iter()
        .any(|row| row.live && row.pid == identity.pid && row.start_time == identity.start_time);
    if tracked {
        Ok(())
    } else {
        Err("process is not a live sampled clud process")
    }
}

pub(super) fn handle_reap(mut request: Request, state_dir: &Path) {
    let parsed = read_body(&mut request)
        .map_err(|err| format!("read body failed: {err}"))
        .and_then(|body| {
            serde_json::from_slice::<ReapRequest>(&body)
                .map_err(|err| format!("invalid JSON: {err}"))
        });
    let payload = match parsed {
        Ok(payload) => payload,
        Err(err) => {
            respond_json(request, 400, json_error_bytes(&err).as_slice());
            return;
        }
    };
    let identity = ProcessIdentity::new(payload.pid, payload.start_time);
    let snapshot = match super::super::client::daemon_client_proc_snapshot_for_dashboard(state_dir)
    {
        Ok(snapshot) => snapshot,
        Err(err) => {
            respond_json(
                request,
                503,
                json_error_bytes(&format!("process sample unavailable: {err}")).as_slice(),
            );
            return;
        }
    };
    if let Err(reason) = authorize(&snapshot, identity) {
        respond_json(request, 403, json_error_bytes(reason).as_slice());
        return;
    }
    let daemons: HashSet<u32> = crate::process_scan::scan_env("CLUD").declared_daemons;
    let protected = [snapshot.sampler_pid, std::process::id()];
    crate::process_tree::kill_tree_filtered_automatic(identity, &|pid| {
        !daemons.contains(&pid) && !protected.contains(&pid)
    });
    eprintln!(
        "[clud] dashboard: reaped process subtree rooted at pid {}",
        identity.pid
    );
    respond_json(request, 200, br#"{"ok":true}"#);
}

#[cfg(test)]
#[path = "http_proc_reap_tests.rs"]
mod tests;
