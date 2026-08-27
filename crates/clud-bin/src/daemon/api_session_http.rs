//! Typed, bearer-authenticated logical-session HTTP DTOs and routing.
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tiny_http::{Method, Request};
use super::api_sessions::{ApiSessionBackend, ApiSessionStore, CreateApiSession, ResolvedApiSessionSettings};
use super::api_session_lifecycle::ApiSessionLifecycle;
use super::http_response::{read_body, respond_json};

#[derive(Deserialize)] struct CreateDto { backend: ApiSessionBackend, cwd: PathBuf, #[serde(default)] name: Option<String>, #[serde(default)] model: Option<String>, #[serde(default)] safe: bool }
#[derive(Serialize)] struct ErrorDto<'a> { code: &'a str, message: &'a str }
fn error(request: Request, status: u16, code: &str, message: &str) { respond_json(request, status, &serde_json::to_vec(&ErrorDto { code, message }).unwrap()); }

pub(super) fn handle(mut request: Request, method: Method, path: &str, state_dir: PathBuf, lifecycle: &ApiSessionLifecycle) {
    let store = ApiSessionStore::new(state_dir);
    if method == Method::Get && path == "/v1/sessions" { let records = store.list().unwrap_or_default(); respond_json(request, 200, &serde_json::to_vec(&records).unwrap()); return; }
    if method == Method::Post && path == "/v1/sessions" { let body = read_body(&mut request).ok().and_then(|b| serde_json::from_slice::<CreateDto>(&b).ok()); let Some(dto) = body else { return error(request, 400, "invalid_request", "expected typed backend and cwd"); }; match store.create(CreateApiSession { backend: dto.backend, cwd: dto.cwd, name: dto.name, resolved_settings: ResolvedApiSessionSettings { model: dto.model, safe: dto.safe, model_provider: None, harness: None, routing_mode: None } }) { Ok(record) => respond_json(request, 201, &serde_json::to_vec(&record).unwrap()), Err(_) => error(request, 400, "invalid_request", "cwd must exist and canonicalize") }; return; }
    let Some(rest) = path.strip_prefix("/v1/sessions/") else { return error(request, 404, "not_found", "API route not found"); };
    let mut parts = rest.split('/'); let Some(id) = parts.next() else { return error(request, 404, "not_found", "session not found"); }; let action = parts.next();
    match (method, action) {
        (Method::Get, None) => match store.get(id) { Ok(record) => respond_json(request, 200, &serde_json::to_vec(&record).unwrap()), Err(_) => error(request, 404, "not_found", "session not found") },
        (Method::Get, Some("events")) => { let after = request.url().split("after=").nth(1).and_then(|v| v.split('&').next()).and_then(|v| v.parse().ok()).unwrap_or(0); match store.events_after(id, after, 512) { Ok(events) => respond_json(request, 200, &serde_json::to_vec(&events).unwrap()), Err(_) => error(request, 404, "not_found", "session not found") } },
        (Method::Post, Some("interrupt")) => match lifecycle.interrupt(id) { Ok(value) => respond_json(request, 200, &serde_json::to_vec(&format!("{value:?}")).unwrap()), Err(_) => error(request, 409, "session_busy", "no active API turn") },
        (Method::Delete, None) => match lifecycle.kill(id) { Ok(_) => respond_json(request, 200, br#"{"status":"terminated"}"#), Err(_) => error(request, 404, "not_found", "session not found") },
        (Method::Post, Some("turns")) => error(request, 409, "resume_unavailable", "turn submission requires the daemon plan controller"),
        _ => error(request, 404, "not_found", "API route not found"),
    }
}
