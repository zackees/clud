use super::*;
use tiny_http::{Header, Response};

pub(super) fn respond_capability_bootstrap(request: Request, access: &DashboardAccess, path: &str) {
    let location = if path.is_empty() { "/" } else { path };
    let response = Response::empty(302)
        .with_header(
            Header::from_bytes(&b"Location"[..], location.as_bytes())
                .expect("redirect path is a valid header"),
        )
        .with_header(
            Header::from_bytes(&b"Set-Cookie"[..], access.cookie_header_value().as_bytes())
                .expect("capability cookie is a valid header"),
        )
        .with_header(no_cache_header());
    let _ = request.respond(response);
}

pub(super) fn respond_html(request: Request, status: u16, body: &[u8]) {
    respond_with_content_type(request, status, body, html_content_type());
}

pub(super) fn respond_json(request: Request, status: u16, body: &[u8]) {
    respond_with_content_type(request, status, body, json_content_type());
}

pub(super) fn respond_text(request: Request, status: u16, body: &[u8]) {
    respond_with_content_type(request, status, body, text_content_type());
}

fn respond_with_content_type(request: Request, status: u16, body: &[u8], content_type: Header) {
    let response = Response::from_data(body.to_vec())
        .with_status_code(status)
        .with_header(content_type)
        .with_header(no_cache_header());
    let _ = request.respond(response);
}

fn html_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
        .expect("static content-type header")
}

fn json_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static content-type header")
}

fn text_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"text/plain; charset=utf-8"[..])
        .expect("static content-type header")
}

fn no_cache_header() -> Header {
    Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).expect("static cache header")
}

pub(super) fn read_body(request: &mut Request) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    request
        .as_reader()
        .take(MAX_REQUEST_BODY_BYTES as u64)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

pub(super) fn json_error_bytes(message: &str) -> Vec<u8> {
    let payload = serde_json::json!({ "error": message });
    serde_json::to_vec(&payload).unwrap_or_else(|_| b"{\"error\":\"unknown\"}".to_vec())
}

pub(super) fn find_body_start(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}
