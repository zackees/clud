use super::*;

#[derive(Debug, Clone)]
pub(super) struct ToolTelemetry {
    server: Option<String>,
    token: Option<String>,
    id: String,
    name: String,
    start_time_ms: u64,
}

#[derive(Debug, Serialize)]
struct ToolTelemetryEvent<'a> {
    event: &'a str,
    id: &'a str,
    name: &'a str,
    start_time_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_tail: Option<&'a str>,
}

impl ToolTelemetry {
    pub(super) fn start(name: &str) -> Self {
        let start_time_ms = current_unix_millis();
        let id = format!("{}-{start_time_ms}", std::process::id());
        let server = std::env::var(ENV_DAEMON_HTTP_SERVER)
            .ok()
            .filter(|value| !value.is_empty());
        let token = std::env::var(ENV_DAEMON_HTTP_TOKEN)
            .ok()
            .filter(|value| !value.is_empty());
        let telemetry = Self {
            server,
            token,
            id,
            name: name.to_string(),
            start_time_ms,
        };
        telemetry.send_start();
        telemetry
    }

    fn send_start(&self) {
        let Some(server) = self.server.clone() else {
            return;
        };
        let Some(token) = self.token.clone() else {
            return;
        };
        let id = self.id.clone();
        let name = self.name.clone();
        let start_time_ms = self.start_time_ms;
        thread::spawn(move || {
            let event = ToolTelemetryEvent {
                event: "start",
                id: &id,
                name: &name,
                start_time_ms,
                end_time_ms: None,
                exit_code: None,
                stderr_tail: None,
            };
            post_tool_telemetry(&server, &token, &event);
        });
    }

    pub(super) fn finish(&self, exit_code: i32, stderr_tail: Option<String>) {
        let Some(server) = self.server.as_ref() else {
            return;
        };
        let Some(token) = self.token.as_ref() else {
            return;
        };
        let stderr_tail = if exit_code == 0 { None } else { stderr_tail };
        let event = ToolTelemetryEvent {
            event: "finish",
            id: &self.id,
            name: &self.name,
            start_time_ms: self.start_time_ms,
            end_time_ms: Some(current_unix_millis()),
            exit_code: Some(exit_code),
            stderr_tail: stderr_tail.as_deref(),
        };
        post_tool_telemetry(server, token, &event);
    }
}

fn post_tool_telemetry(server: &str, token: &str, event: &ToolTelemetryEvent<'_>) {
    let Ok(body) = serde_json::to_vec(event) else {
        return;
    };
    let url = format!("{}/tools/event", server.trim_end_matches('/'));
    let _ = ureq::AgentBuilder::new()
        .timeout(TOOL_TELEMETRY_TIMEOUT)
        .build()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("Host", &crate::log_event::dashboard_host_header(server))
        .set("Cookie", &format!("clud_dashboard_token={token}"))
        .send_bytes(&body);
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn stderr_tail_200(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let mut tail: Vec<char> = text.chars().rev().take(200).collect();
    tail.reverse();
    Some(tail.into_iter().collect())
}

#[cfg(test)]
#[path = "tool_run_tests.rs"]
mod tests;
