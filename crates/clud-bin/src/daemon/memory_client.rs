//! Issue #262: HTTP client used by the `clud memory *` CLI verbs.
//!
//! Talks to the dashboard's `/memory/*` routes (loopback, no auth). All
//! calls go through the daemon so there is exactly one SQLite writer per
//! process (DD-018). Each helper returns a tuple of `(status, body)` —
//! callers map status → exit code and decode the JSON body.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use super::http::read_dashboard_port;

const READ_WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// Result of one HTTP call against the dashboard's memory routes.
pub struct MemoryHttpResponse {
    pub status: u16,
    pub body: String,
}

/// `GET /memory/stats`.
pub fn http_stats(state_dir: &Path) -> io::Result<MemoryHttpResponse> {
    fetch(state_dir, "GET", "/memory/stats", None)
}

/// `GET /memory/recent?limit=<n>`.
pub fn http_recent(state_dir: &Path, limit: usize) -> io::Result<MemoryHttpResponse> {
    let path = format!("/memory/recent?limit={limit}");
    fetch(state_dir, "GET", &path, None)
}

/// `GET /memory/search?q=...&k=...&...`.
pub fn http_search(
    state_dir: &Path,
    query: &str,
    k: u32,
    session_id: Option<&str>,
    tier_floor: Option<&str>,
    scope_key: Option<&str>,
) -> io::Result<MemoryHttpResponse> {
    let mut path = format!("/memory/search?q={}&k={}", url_encode(query), k.max(1));
    if let Some(s) = session_id {
        path.push_str("&session_id=");
        path.push_str(&url_encode(s));
    }
    if let Some(t) = tier_floor {
        path.push_str("&tier_floor=");
        path.push_str(&url_encode(t));
    }
    if let Some(sk) = scope_key {
        path.push_str("&scope_key=");
        path.push_str(&url_encode(sk));
    }
    fetch(state_dir, "GET", &path, None)
}

/// `POST /memory/save` with a JSON body.
pub fn http_save(state_dir: &Path, payload: &str) -> io::Result<MemoryHttpResponse> {
    fetch(state_dir, "POST", "/memory/save", Some(payload))
}

/// `POST /memory/forget/<id>`.
pub fn http_forget(state_dir: &Path, id: &str) -> io::Result<MemoryHttpResponse> {
    let path = format!("/memory/forget/{}", url_encode(id));
    fetch(state_dir, "POST", &path, Some("{}"))
}

fn fetch(
    state_dir: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> io::Result<MemoryHttpResponse> {
    let port = read_dashboard_port(state_dir)?
        .ok_or_else(|| io::Error::other("daemon has no dashboard listener"))?;
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(READ_WRITE_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_WRITE_TIMEOUT))?;

    let mut req = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            b.len(),
            b
        ));
    } else {
        req.push_str("\r\n");
    }
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf)?;
    let status_line_end = buf
        .windows(2)
        .position(|w| w == b"\r\n")
        .or_else(|| buf.windows(1).position(|w| w == b"\n"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no status line"))?;
    let status_line = std::str::from_utf8(&buf[..status_line_end])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no status code"))?;
    let body_start = find_body_start(&buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no header terminator"))?;
    let body = String::from_utf8(buf[body_start..].to_vec())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(MemoryHttpResponse { status, body })
}

fn find_body_start(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            let bytes = c.encode_utf8(&mut buf).as_bytes();
            for b in bytes {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_passes_unreserved() {
        assert_eq!(url_encode("abc123_-.~"), "abc123_-.~");
    }

    #[test]
    fn url_encode_escapes_spaces_and_quotes() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn find_body_start_picks_crlf_or_lf() {
        let crlf = b"HTTP/1.0 200 OK\r\n\r\nbody";
        let lf = b"HTTP/1.0 200 OK\n\nbody";
        assert_eq!(&crlf[find_body_start(crlf).unwrap()..], b"body");
        assert_eq!(&lf[find_body_start(lf).unwrap()..], b"body");
    }
}
