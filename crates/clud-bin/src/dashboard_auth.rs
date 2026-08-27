//! Capability authentication for the daemon's loopback dashboard.
//!
//! Loopback binding is not a browser security boundary: a DNS-rebound page can
//! address `127.0.0.1`. Every dashboard request therefore needs the daemon's
//! per-start capability, delivered to browsers once through a Strict HttpOnly
//! cookie.

use base64::Engine;

pub const COOKIE_NAME: &str = "clud_dashboard_token";

pub fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable for dashboard capability");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Constant-time comparison for the programmatic loopback API bearer.
pub fn allows_bearer(expected: &str, authorization: Option<&str>) -> bool {
    let Some(actual) = authorization.and_then(|value| value.strip_prefix("Bearer ")) else {
        return false;
    };
    let mut different = expected.len() ^ actual.len();
    for (left, right) in expected.bytes().zip(actual.bytes()) {
        different |= usize::from(left ^ right);
    }
    different == 0
}

#[derive(Debug, Clone)]
pub struct DashboardAccess {
    token: String,
}

impl DashboardAccess {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    pub fn allows_host(&self, host: Option<&str>, port: u16) -> bool {
        let Some(host) = host else {
            return false;
        };
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    }

    pub fn allows_token(&self, query_token: Option<&str>, cookie: Option<&str>) -> bool {
        query_token.is_some_and(|token| token == self.token)
            || cookie
                .and_then(|raw| {
                    raw.split(';').map(str::trim).find_map(|part| {
                        let (name, value) = part.split_once('=')?;
                        (name == COOKIE_NAME).then_some(value)
                    })
                })
                .is_some_and(|token| token == self.token)
    }

    pub fn cookie_header_value(&self) -> String {
        format!(
            "{COOKIE_NAME}={}; HttpOnly; SameSite=Strict; Path=/",
            self.token
        )
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}
