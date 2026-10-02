//! The broker's only path to GitHub: the real `gh api -i` (#1743).
//!
//! Running `gh` keeps authentication, enterprise hosts and proxies exactly
//! `gh`'s. The broker never holds a token; it forwards the caller's
//! [`super::FORWARDED_ENV`] to the child and parses the status line, headers
//! and body that `--include` prints.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use running_process::{NativeProcess, ProcessConfig, StderrMode, StdinMode, StreamKind};

/// One HTTP response, as `gh api -i` reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Arc<Vec<u8>>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct UpstreamRequest<'a> {
    pub gh: &'a Path,
    pub endpoint: &'a str,
    pub hostname: Option<&'a str>,
    pub env: &'a [(String, String)],
    pub if_none_match: Option<&'a str>,
    pub if_modified_since: Option<&'a str>,
}

/// Fetches one endpoint. The production implementation is [`GhCli`]; tests
/// inject a counting fake.
pub trait Upstream: Send + Sync {
    fn fetch(&self, request: &UpstreamRequest<'_>) -> Result<Response, String>;
}

/// Responses larger than this are not brokered; the shim falls back.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Daemon env the upstream `gh` must not see: debug output, pagers, and
/// anything that forces TTY-style color or formatting onto a pipe.
const STRIPPED_ENV: &[&str] = &[
    "GH_DEBUG",
    "DEBUG",
    "GH_PAGER",
    "PAGER",
    "GH_FORCE_TTY",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "NO_COLOR",
];

pub struct GhCli;

impl GhCli {
    pub fn argv(request: &UpstreamRequest<'_>) -> Vec<String> {
        let mut argv = vec![
            request.gh.to_string_lossy().into_owned(),
            "api".to_string(),
            "-i".to_string(),
            request.endpoint.to_string(),
        ];
        if let Some(host) = request.hostname {
            argv.extend(["--hostname".to_string(), host.to_string()]);
        }
        if let Some(etag) = request.if_none_match {
            argv.extend(["-H".to_string(), format!("If-None-Match: {etag}")]);
        }
        if let Some(stamp) = request.if_modified_since {
            argv.extend(["-H".to_string(), format!("If-Modified-Since: {stamp}")]);
        }
        argv
    }

    /// The daemon's own env with every forwarded key replaced by the
    /// caller's value (or removed), plus non-interactive `gh` switches.
    pub fn env(forwarded: &[(String, String)]) -> Vec<(String, String)> {
        // `vars_os`: `std::env::vars` panics on one non-UTF-8 variable.
        let mut env: Vec<(String, String)> = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .filter(|(key, _)| {
                !super::FORWARDED_ENV
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(key))
                    && !STRIPPED_ENV.contains(&key.as_str())
            })
            .collect();
        env.extend(
            forwarded
                .iter()
                .filter(|(key, _)| super::FORWARDED_ENV.contains(&key.as_str()))
                .cloned(),
        );
        env.push(("GH_PROMPT_DISABLED".into(), "1".into()));
        // The parsed `-i` output must be plain: no ANSI in header names,
        // no pretty-printed body to cache and replay.
        env.push(("NO_COLOR".into(), "1".into()));
        env.push(("CLICOLOR_FORCE".into(), "0".into()));
        env.push(("GH_NO_UPDATE_NOTIFIER".into(), "1".into()));
        env
    }
}

impl Upstream for GhCli {
    fn fetch(&self, request: &UpstreamRequest<'_>) -> Result<Response, String> {
        let process = NativeProcess::new(ProcessConfig {
            command: crate::subprocess::command_spec_for_subprocess(Self::argv(request)),
            cwd: None,
            env: Some(Self::env(request.env)),
            capture: true,
            // stdout carries the response; stderr only `gh`'s complaint.
            stderr_mode: StderrMode::Pipe,
            creationflags: crate::win_creation_flags::invisible_helper_creationflags(),
            create_process_group: false,
            stdin_mode: StdinMode::Null,
            nice: None,
            address_space_limit_bytes: None,
        });
        process
            .start()
            .map_err(|error| format!("start gh: {error}"))?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match process.wait(Some(Duration::from_millis(100))) {
                Ok(_) => break,
                Err(running_process::ProcessError::Timeout) if Instant::now() < deadline => {
                    if process.captured_stream_bytes(StreamKind::Stdout) > MAX_RESPONSE_BYTES {
                        let _ = process.kill();
                        return Err("gh response too large to broker".into());
                    }
                }
                Err(error) => {
                    let _ = process.kill();
                    return Err(format!("gh api: {error}"));
                }
            }
        }
        let stdout = process.drain_stream_raw(StreamKind::Stdout);
        if stdout.len() > MAX_RESPONSE_BYTES {
            return Err("gh response too large to broker".into());
        }
        parse_include(&stdout).ok_or_else(|| {
            let stderr = process.drain_stream_raw(StreamKind::Stderr);
            let text = String::from_utf8_lossy(&stderr);
            format!("gh api printed no response: {}", text.trim())
        })
    }
}

/// Parse `gh api -i` stdout: `HTTP/x.y CODE Reason\n`, then
/// `Name: value\r\n` lines, a blank `\r\n`, and the raw body.
pub fn parse_include(stdout: &[u8]) -> Option<Response> {
    let line_end = stdout.iter().position(|&b| b == b'\n')?;
    let status_line = std::str::from_utf8(&stdout[..line_end]).ok()?.trim_end();
    let mut parts = status_line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = parts.next()?.parse().ok()?;
    let rest = &stdout[line_end + 1..];
    let (head, body) = if let Some(body) = rest.strip_prefix(b"\r\n") {
        (&rest[..0], body)
    } else {
        let split = rest.windows(4).position(|w| w == b"\r\n\r\n")?;
        (&rest[..split], &rest[split + 4..])
    };
    let head = std::str::from_utf8(head).ok()?;
    let headers = head
        .split("\r\n")
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split_once(':')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Response {
        status,
        headers,
        body: Arc::new(body.to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gh_include_output_byte_exact() {
        let out = b"HTTP/2.0 200 OK\nContent-Type: application/json; charset=utf-8\r\nEtag: W/\"abc\"\r\nX-Ratelimit-Remaining: 4999\r\n\r\n{\"a\":\"x\\r\\n\"}\r\n\xff";
        let r = parse_include(out).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.header("etag"), Some("W/\"abc\""));
        assert_eq!(r.header("x-ratelimit-remaining"), Some("4999"));
        assert_eq!(r.body.as_slice(), b"{\"a\":\"x\\r\\n\"}\r\n\xff");
    }

    #[test]
    fn parses_a_304_without_a_body_and_rejects_noise() {
        let r = parse_include(b"HTTP/2.0 304 Not Modified\nEtag: \"e\"\r\n\r\n").unwrap();
        assert_eq!(r.status, 304);
        assert!(r.body.is_empty());
        let r = parse_include(b"HTTP/1.1 204 No Content\n\r\n").unwrap();
        assert_eq!(r.status, 204);
        assert!(r.headers.is_empty());
        assert!(parse_include(b"").is_none());
        assert!(parse_include(b"error connecting to api.github.com\n").is_none());
        assert!(parse_include(b"HTTP/2.0 200 OK\nbroken header\r\n\r\n").is_none());
    }

    #[test]
    fn argv_carries_conditional_headers_and_hostname() {
        let gh = Path::new("/usr/bin/gh");
        let argv = GhCli::argv(&UpstreamRequest {
            gh,
            endpoint: "repos/o/r",
            hostname: Some("ghe.example.com"),
            env: &[],
            if_none_match: Some("\"e\""),
            if_modified_since: None,
        });
        assert_eq!(
            argv,
            [
                "/usr/bin/gh",
                "api",
                "-i",
                "repos/o/r",
                "--hostname",
                "ghe.example.com",
                "-H",
                "If-None-Match: \"e\""
            ]
        );
    }

    #[test]
    fn env_forwards_only_the_callers_gh_keys() {
        let env = GhCli::env(&[
            ("GH_TOKEN".into(), "t".into()),
            ("NOT_FORWARDED".into(), "x".into()),
        ]);
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("GH_TOKEN").as_deref(), Some("t"));
        assert_eq!(get("NOT_FORWARDED"), None);
        assert_eq!(get("GH_PROMPT_DISABLED").as_deref(), Some("1"));
        assert_eq!(env.iter().filter(|(k, _)| k == "GH_TOKEN").count(), 1);
        assert_eq!(get("NO_COLOR").as_deref(), Some("1"));
        assert_eq!(get("CLICOLOR_FORCE").as_deref(), Some("0"));
        assert_eq!(get("GH_FORCE_TTY"), None);
        assert_eq!(env.iter().filter(|(k, _)| k == "NO_COLOR").count(), 1);
    }
}
