//! Typed, bearer-authenticated logical-session HTTP DTOs and routing.
//!
//! The boundary accepts stable JSON only. Provider argv, environment, and
//! per-turn cwd values never cross it.

use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};
use tiny_http::{Method, Request};

use crate::args::Args;
use crate::backend::{Backend, HarnessSelection, ModelProvider, PreferenceSource, ResolvedLaunchTarget, RoutingMode};
use crate::command::{HeadlessSession, HeadlessTurnRequest, LaunchPlan};

use super::api_session_lifecycle::{ApiSessionLifecycle, LifecycleError, LifecycleReply};
use super::api_sessions::{ApiSessionBackend, ApiSessionEvent, ApiSessionRecord, ApiSessionStore, ApiSessionStoreError, CreateApiSession, ResolvedApiSessionSettings, DEFAULT_EVENT_LIMIT};
use super::headless_adapter::build_turn_plan;
use super::http_response::{read_body, respond_json};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_EVENTS_PAGE: usize = 128;

#[derive(Deserialize)]
struct CreateDto { backend: ApiSessionBackend, cwd: PathBuf, #[serde(default)] name: Option<String>, #[serde(default)] model: Option<String>, #[serde(default)] safe: bool }
#[derive(Deserialize)]
struct TurnDto { message: String, #[serde(default)] interrupt_running: bool }
#[derive(Serialize)]
struct ErrorDto<'a> { code: &'a str, message: &'a str }
#[derive(Serialize)]
struct TurnResponse<'a> { session_id: &'a str, turn_id: String, #[serde(skip_serializing_if = "Option::is_none")] generation: Option<u64>, status: &'a str }
#[derive(Serialize)]
struct InterruptResponse { status: &'static str, forced: bool }
#[derive(Serialize)]
struct EventsResponse { events: Vec<ApiSessionEvent>, next_cursor: u64, retention_gap: bool }

fn events_query(url: &str, first_available: u64) -> Result<(u64, usize, bool), &'static str> {
    let mut after = 0_u64;
    let mut limit = MAX_EVENTS_PAGE;
    if let Some((_, query)) = url.split_once('?') {
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').ok_or("query parameters must use key=value")?;
            match key {
                "after" => after = value.parse().map_err(|_| "after must be an unsigned cursor")?,
                "limit" => { limit = value.parse().map_err(|_| "limit must be an integer")?; if limit == 0 || limit > MAX_EVENTS_PAGE { return Err("limit must be between 1 and 128"); } }
                _ => return Err("unknown events query parameter"),
            }
        }
    }
    Ok((after, limit, after.saturating_add(1) < first_available))
}

fn error(request: Request, status: u16, code: &str, message: &str) { respond_json(request, status, &serde_json::to_vec(&ErrorDto { code, message }).unwrap_or_else(|_| b"{}".to_vec())); }
fn header(request: &Request, name: &'static str) -> Option<String> { request.headers().iter().find(|value| value.field.equiv(name)).map(|value| value.value.as_str().to_string()) }

fn store_error(request: Request, value: ApiSessionStoreError) {
    match value {
        ApiSessionStoreError::NotFound(_) => error(request, 404, "not_found", "session not found"),
        ApiSessionStoreError::InvalidCwd { .. } | ApiSessionStoreError::InvalidTransition { .. } => error(request, 400, "invalid_request", "request violates the session contract"),
        ApiSessionStoreError::Corrupt { .. } => error(request, 409, "corrupt_session", "session state is unreadable"),
        ApiSessionStoreError::IdempotencyConflict { .. } => error(request, 409, "idempotency_conflict", "Idempotency-Key was reused with a different request"),
        ApiSessionStoreError::EventTooLarge { .. } | ApiSessionStoreError::Io(_) => error(request, 500, "internal_error", "daemon session storage failed"),
    }
}

fn lifecycle_error(request: Request, value: LifecycleError) {
    match value {
        LifecycleError::NotFound => error(request, 404, "not_found", "session not found"),
        LifecycleError::Terminated => error(request, 409, "session_terminated", "session has been terminated"),
        LifecycleError::ResumeUnavailable => error(request, 409, "resume_unavailable", "a provider session identity has not been captured"),
        LifecycleError::IdempotencyConflict => error(request, 409, "idempotency_conflict", "Idempotency-Key was reused with a different request"),
        LifecycleError::Launch(_) => error(request, 422, "launch_rejected", "stored settings cannot produce a headless plan"),
    }
}

fn new_claude_session_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "could not allocate a provider session identity".to_string())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]))
}

fn turn_plan(record: &ApiSessionRecord, message: String) -> Result<LaunchPlan, String> {
    if message.trim().is_empty() || message.len() > MAX_MESSAGE_BYTES { return Err("message must be non-empty and at most 64 KiB".to_string()); }
    let backend = match record.backend { ApiSessionBackend::Claude => Backend::Claude, ApiSessionBackend::Codex => Backend::Codex };
    let mut argv = vec!["clud".to_string()];
    if record.resolved_settings.safe { argv.push("--safe".to_string()); }
    if let Some(model) = &record.resolved_settings.model { argv.extend(["--model".to_string(), model.clone()]); }
    let args = Args::try_parse_from(argv).map_err(|_| "stored settings are invalid".to_string())?;
    let provider = record.resolved_settings.model_provider.as_deref().and_then(ModelProvider::from_settings_str).unwrap_or_else(|| backend.as_model_provider());
    let harness = record.resolved_settings.harness.as_deref().and_then(HarnessSelection::from_settings_str).unwrap_or_else(|| HarnessSelection::for_backend(backend));
    let routing_mode = match record.resolved_settings.routing_mode.as_deref() { Some("unified") => RoutingMode::Unified, _ => RoutingMode::Direct };
    let target = ResolvedLaunchTarget { routing_mode, model_provider: provider, requested_harness: harness, effective_harness: backend, provider_source: PreferenceSource::BuiltInDefault, harness_source: PreferenceSource::BuiltInDefault };
    let session = match (record.generation, record.provider_session_id.as_ref()) {
        (0, _) if backend == Backend::Claude => HeadlessSession::Initial { claude_session_id: Some(new_claude_session_id()?) },
        (0, _) => HeadlessSession::Initial { claude_session_id: None },
        (_, Some(id)) => HeadlessSession::Resume { provider_session_id: id.clone() },
        (_, None) => return Err("provider identity is required to resume".to_string()),
    };
    build_turn_plan(&args, target, backend.executable_name(), &HeadlessTurnRequest { message, cwd: record.cwd.clone(), session })
}

fn fingerprint(message: &str, interrupt_running: bool) -> String { format!("v1:{}:{message}", interrupt_running as u8) }

pub(super) fn handle(mut request: Request, method: Method, path: &str, state_dir: PathBuf, lifecycle: &ApiSessionLifecycle) {
    let store = ApiSessionStore::new(state_dir);
    if method == Method::Get && path == "/v1/sessions" { return match store.list() { Ok(records) => respond_json(request, 200, &serde_json::to_vec(&records).unwrap_or_else(|_| b"[]".to_vec())), Err(value) => store_error(request, value) }; }
    if method == Method::Post && path == "/v1/sessions" {
        let Some(body) = read_body(&mut request).ok().and_then(|body| serde_json::from_slice::<CreateDto>(&body).ok()) else { return error(request, 400, "invalid_request", "expected typed backend and cwd") };
        return match store.create(CreateApiSession { backend: body.backend, cwd: body.cwd, name: body.name, resolved_settings: ResolvedApiSessionSettings { model: body.model, safe: body.safe, model_provider: None, harness: None, routing_mode: None } }) { Ok(record) => respond_json(request, 201, &serde_json::to_vec(&record).unwrap_or_else(|_| b"{}".to_vec())), Err(value) => store_error(request, value) };
    }
    let Some(rest) = path.strip_prefix("/v1/sessions/") else { return error(request, 404, "not_found", "API route not found") };
    let mut parts = rest.split('/'); let Some(id) = parts.next().filter(|id| !id.is_empty()) else { return error(request, 404, "not_found", "session not found") }; let action = parts.next();
    if parts.next().is_some() { return error(request, 404, "not_found", "API route not found"); }
    match (method, action) {
        (Method::Get, None) => match store.get(id) { Ok(record) => respond_json(request, 200, &serde_json::to_vec(&record).unwrap_or_else(|_| b"{}".to_vec())), Err(value) => store_error(request, value) },
        (Method::Get, Some("events")) => {
            let record = match store.get(id) { Ok(record) => record, Err(value) => return store_error(request, value) };
            let first_available = record.events.front().map(|event| event.cursor).unwrap_or(record.next_event_cursor);
            let (after, limit, retention_gap) = match events_query(request.url(), first_available) { Ok(value) => value, Err(message) => return error(request, 400, "invalid_cursor", message) };
            let events = record.events.into_iter().filter(|event| event.cursor > after).take(limit.min(DEFAULT_EVENT_LIMIT)).collect::<Vec<_>>();
            let next_cursor = events.last().map(|event| event.cursor).unwrap_or(after);
            respond_json(request, 200, &serde_json::to_vec(&EventsResponse { events, next_cursor, retention_gap }).unwrap_or_else(|_| b"{}".to_vec()))
        }
        (Method::Post, Some("interrupt")) => match lifecycle.interrupt(id) { Ok(LifecycleReply::Interrupted { forced }) => respond_json(request, 200, &serde_json::to_vec(&InterruptResponse { status: "interrupted", forced }).unwrap()), Ok(_) => error(request, 409, "session_not_running", "no active API turn"), Err(LifecycleError::NotFound) => error(request, 404, "not_found", "session not found"), Err(_) => error(request, 409, "session_not_running", "no active API turn") },
        (Method::Delete, None) => match lifecycle.kill(id) { Ok(_) => respond_json(request, 200, br#"{"status":"terminated"}"#), Err(value) => lifecycle_error(request, value) },
        (Method::Post, Some("turns")) => {
            let key = header(&request, "Idempotency-Key");
            let Some(body) = read_body(&mut request).ok().and_then(|body| serde_json::from_slice::<TurnDto>(&body).ok()) else { return error(request, 400, "invalid_request", "expected message and optional interrupt_running") };
            let record = match store.get(id) { Ok(record) => record, Err(value) => return store_error(request, value) };
            let plan = match turn_plan(&record, body.message.clone()) { Ok(plan) => plan, Err(message) if message.contains("provider identity") => return error(request, 409, "resume_unavailable", "a provider session identity has not been captured"), Err(_) => return error(request, 400, "invalid_request", "message or stored session settings are invalid") };
            match lifecycle.submit(id, plan, key, fingerprint(&body.message, body.interrupt_running), body.interrupt_running) {
                Ok(LifecycleReply::Started { generation, turn_id }) => respond_json(request, 202, &serde_json::to_vec(&TurnResponse { session_id: id, turn_id, generation: Some(generation), status: "started" }).unwrap()),
                Ok(LifecycleReply::Replayed { turn_id }) => respond_json(request, 200, &serde_json::to_vec(&TurnResponse { session_id: id, turn_id, generation: None, status: "replayed" }).unwrap()),
                Ok(LifecycleReply::SessionBusy) => error(request, 409, "session_busy", "an API turn is already active"),
                Ok(_) => error(request, 409, "session_busy", "session cannot accept a turn"),
                Err(value) => lifecycle_error(request, value),
            }
        }
        _ => error(request, 404, "not_found", "API route not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_query_is_bounded_and_reports_retention_gap() {
        assert_eq!(events_query("/v1/sessions/a/events?after=2&limit=3", 9).unwrap(), (2, 3, true));
        assert!(events_query("/v1/sessions/a/events?after=no", 1).is_err());
        assert!(events_query("/v1/sessions/a/events?limit=129", 1).is_err());
        assert!(events_query("/v1/sessions/a/events?cursor=1", 1).is_err());
    }
}
