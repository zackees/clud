//! End-to-end request pipeline for the Codex bridge (issue #627 phase 3,
//! step 5).
//!
//! Chains the four pieces built in steps 1-4 into one call:
//!
//! ```text
//! Anthropic request -> codex_translate -> codex_upstream -> codex_sse -> Anthropic SSE
//! ```
//!
//! Upstream is *always* streamed, even for a non-streaming Messages request.
//! Aggregating the translated Anthropic events into a final `Message` reuses
//! the state machine that step 3 already fuzzed, rather than adding a second,
//! separately-wrong mapping for the non-streaming shape.

use std::sync::atomic::AtomicBool;

use crate::codex_sse::{FrameDecoder, StreamTranslator};
use crate::codex_translate::{translate_bytes, TranslateError, DEFAULT_CODEX_MODEL};
use crate::codex_upstream::{CredentialSource, UpstreamClient, UpstreamError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    Translate(TranslateError),
    Upstream(UpstreamError),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Translate(error) => write!(formatter, "{error}"),
            Self::Upstream(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PipelineError {}

impl PipelineError {
    /// Downstream HTTP status for a failure that happens *before* any output.
    ///
    /// Once a frame has been flushed there is no status left to choose, which
    /// is why the streaming path reports late failures as an SSE `error` event
    /// instead (see [`StreamTranslator::fail`]).
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Translate(TranslateError::Invalid(_)) => 400,
            // The request is well-formed but names something we refuse to
            // approximate; 422 keeps that distinct from a parse failure.
            Self::Translate(TranslateError::Unsupported(_)) => 422,
            Self::Upstream(UpstreamError::Credentials(_)) => 401,
            Self::Upstream(UpstreamError::Status(status)) => match status {
                400 | 401 | 403 | 404 | 413 | 422 | 429 => *status,
                _ => 502,
            },
            Self::Upstream(UpstreamError::Timeout) => 504,
            Self::Upstream(_) => 502,
        }
    }

    /// A message safe to hand to the harness. Upstream bodies never appear:
    /// they carry account identifiers and key fragments.
    pub fn client_message(&self) -> String {
        match self {
            // Translation messages are ours and name only the offending
            // feature, so they are worth surfacing -- that is the difference
            // between "the bridge refused top_k" and an opaque 422.
            Self::Translate(error) => error.to_string(),
            Self::Upstream(UpstreamError::Credentials(_)) => {
                "the Codex bridge has no upstream credentials".to_string()
            }
            Self::Upstream(UpstreamError::Timeout) => "upstream request timed out".to_string(),
            Self::Upstream(UpstreamError::Status(status)) => {
                format!("upstream provider returned status {status}")
            }
            Self::Upstream(_) => "upstream provider error".to_string(),
        }
    }
}

/// Accumulates translated Anthropic SSE frames into one `Message` object.
///
/// Anthropic's non-streaming reply is exactly the stream's fixed point, so
/// this walks the same events rather than introducing a parallel mapping.
#[derive(Debug, Default)]
pub struct MessageAggregator {
    id: String,
    model: String,
    blocks: Vec<serde_json::Value>,
    text: Vec<String>,
    partial_json: Vec<String>,
    stop_reason: String,
    input_tokens: u64,
    output_tokens: u64,
    errored: bool,
}

impl MessageAggregator {
    pub fn new() -> Self {
        Self {
            stop_reason: "end_turn".to_string(),
            ..Self::default()
        }
    }

    pub fn push_frame(&mut self, frame: &str) {
        let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("message_start") => {
                if let Some(message) = value.get("message") {
                    self.id = string_at(message, "id");
                    self.model = string_at(message, "model");
                    self.input_tokens = message
                        .pointer("/usage/input_tokens")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                }
            }
            Some("content_block_start") => {
                self.flush_open_block();
                if let Some(block) = value.get("content_block") {
                    self.blocks.push(block.clone());
                }
            }
            Some("content_block_delta") => {
                match value.pointer("/delta/type").and_then(|kind| kind.as_str()) {
                    Some("text_delta") => self.text.push(string_at_pointer(&value, "/delta/text")),
                    Some("input_json_delta") => self
                        .partial_json
                        .push(string_at_pointer(&value, "/delta/partial_json")),
                    _ => {}
                }
            }
            Some("content_block_stop") => self.flush_open_block(),
            Some("message_delta") => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                    self.stop_reason = reason.to_string();
                }
                if let Some(tokens) = value
                    .pointer("/usage/output_tokens")
                    .and_then(serde_json::Value::as_u64)
                {
                    self.output_tokens = tokens;
                }
                // message_start carried a placeholder; the real count only
                // arrives with the terminal usage.
                if let Some(tokens) = value
                    .pointer("/usage/input_tokens")
                    .and_then(serde_json::Value::as_u64)
                {
                    self.input_tokens = tokens;
                }
            }
            Some("error") => self.errored = true,
            _ => {}
        }
    }

    /// Fold the deltas collected since the last block boundary into the block
    /// they belong to.
    fn flush_open_block(&mut self) {
        let text = std::mem::take(&mut self.text).concat();
        let json = std::mem::take(&mut self.partial_json).concat();
        let Some(block) = self.blocks.last_mut() else {
            return;
        };
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if !text.is_empty() {
                    block["text"] = serde_json::json!(text);
                }
            }
            Some("tool_use") => {
                // Only write when something was buffered. `finish` may flush a
                // block that `content_block_stop` already closed, and an
                // unconditional write would clobber the parsed arguments with
                // the empty-buffer reading.
                if !json.is_empty() {
                    // Fragments are only valid JSON once joined. An unparseable
                    // join means the turn was truncated; an empty object is the
                    // safe reading, since a malformed `input` would make the
                    // client reject a turn it could otherwise partly use.
                    block["input"] = serde_json::from_str(&json).unwrap_or(serde_json::json!({}));
                }
            }
            _ => {}
        }
    }

    pub fn errored(&self) -> bool {
        self.errored
    }

    pub fn finish(mut self) -> serde_json::Value {
        self.flush_open_block();
        serde_json::json!({
            "id": self.id,
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": self.blocks,
            "stop_reason": self.stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
            },
        })
    }
}

fn string_at(value: &serde_json::Value, field: &str) -> String {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn string_at_pointer(value: &serde_json::Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub struct Pipeline<C: CredentialSource> {
    client: UpstreamClient<C>,
    default_model: String,
}

impl<C: CredentialSource> std::fmt::Debug for Pipeline<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pipeline")
            .field("default_model", &self.default_model)
            .finish()
    }
}

impl<C: CredentialSource> Pipeline<C> {
    pub fn new(client: UpstreamClient<C>) -> Self {
        Self {
            client,
            default_model: DEFAULT_CODEX_MODEL.to_string(),
        }
    }

    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Translate, send, and stream the reply as Anthropic SSE frames.
    ///
    /// `sink` sees each frame as it is produced; the caller writes and flushes
    /// it so the turn renders progressively.
    pub fn stream(
        &self,
        request_body: &[u8],
        message_id: &str,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(&str) -> Result<(), UpstreamError>,
    ) -> Result<(), PipelineError> {
        let (upstream_body, model) = self.prepare(request_body)?;

        let mut decoder = FrameDecoder::new();
        let mut translator = StreamTranslator::new(model, message_id);
        let mut pump = |chunk: &[u8]| -> Result<(), UpstreamError> {
            for frame in decoder.push(chunk) {
                for out in translator.push(&frame) {
                    sink(&out)?;
                }
            }
            Ok(())
        };

        let result = self.client.stream(&upstream_body, cancel, &mut pump);
        match result {
            Ok(_) => {
                for frame in decoder.finish() {
                    for out in translator.push(&frame) {
                        sink(&out).map_err(PipelineError::Upstream)?;
                    }
                }
                for out in translator.finish() {
                    sink(&out).map_err(PipelineError::Upstream)?;
                }
                Ok(())
            }
            Err(error) => {
                // A failure after the first frame cannot change the status, so
                // it is reported in-band and the turn is closed cleanly rather
                // than left hanging.
                if translator.is_finished() {
                    return Err(PipelineError::Upstream(error));
                }
                for out in translator.fail() {
                    let _ = sink(&out);
                }
                Err(PipelineError::Upstream(error))
            }
        }
    }

    /// Same path, aggregated into a single Anthropic `Message`.
    pub fn complete(
        &self,
        request_body: &[u8],
        message_id: &str,
        cancel: &AtomicBool,
    ) -> Result<serde_json::Value, PipelineError> {
        let mut aggregator = MessageAggregator::new();
        let mut sink = |frame: &str| -> Result<(), UpstreamError> {
            aggregator.push_frame(frame);
            Ok(())
        };
        self.stream(request_body, message_id, cancel, &mut sink)?;
        if aggregator.errored() {
            return Err(PipelineError::Upstream(UpstreamError::Transport(
                "upstream stream failed",
            )));
        }
        Ok(aggregator.finish())
    }

    /// Translate the request and force streaming upstream.
    fn prepare(&self, request_body: &[u8]) -> Result<(Vec<u8>, String), PipelineError> {
        let target = self
            .client
            .credentials()
            .resolve()
            .map_err(PipelineError::Upstream)?;
        let default_model = target
            .model_override()
            .unwrap_or(self.default_model.as_str())
            .to_string();
        let mut translated =
            translate_bytes(request_body, &default_model).map_err(PipelineError::Translate)?;
        // Always stream upstream: `complete` folds the events back into one
        // message rather than maintaining a second mapping.
        translated.stream = true;
        let model = translated.model.clone();
        let body = serde_json::to_vec(&translated).map_err(|error| {
            PipelineError::Translate(TranslateError::Invalid(error.to_string()))
        })?;
        Ok((body, model))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex_upstream::{ApiKeyCredentials, UpstreamConfig, UpstreamTarget};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Minimal Responses-shaped server: records the request body and replies
    /// with a scripted SSE stream.
    struct FakeResponses {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
        shutdown: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeResponses {
        fn start(reply: Vec<u8>) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let shutdown = Arc::new(AtomicBool::new(false));
            let thread_requests = Arc::clone(&requests);
            let thread_shutdown = Arc::clone(&shutdown);
            let handle = std::thread::spawn(move || {
                while !thread_shutdown.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut raw = Vec::new();
                    let mut byte = [0_u8; 1];
                    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut byte) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => raw.push(byte[0]),
                        }
                    }
                    let head = String::from_utf8_lossy(&raw).to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    let mut body = vec![0_u8; length];
                    if length > 0 {
                        let _ = stream.read_exact(&mut body);
                    }
                    thread_requests
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&body).to_string());
                    let _ = stream.write_all(&reply);
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });
            Self {
                base_url: format!("http://{addr}"),
                requests,
                shutdown,
                handle: Some(handle),
            }
        }

        fn request(&self) -> serde_json::Value {
            let raw = self.requests.lock().unwrap().first().cloned().unwrap();
            serde_json::from_str(&raw).expect("upstream request must be JSON")
        }
    }

    impl Drop for FakeResponses {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn sse(events: &[serde_json::Value]) -> Vec<u8> {
        let body: String = events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap_or("message")
                )
            })
            .collect();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn pipeline(base_url: &str) -> Pipeline<ApiKeyCredentials> {
        Pipeline::new(UpstreamClient::new(
            ApiKeyCredentials::new("sk-test", Some(base_url.to_string())).unwrap(),
            UpstreamConfig {
                connect_timeout: Duration::from_secs(2),
                read_timeout: Duration::from_secs(2),
                overall_timeout: Duration::from_secs(10),
                retry_delay: Duration::from_millis(10),
                ..UpstreamConfig::default()
            },
        ))
    }

    fn text_reply() -> Vec<u8> {
        sse(&[
            json!({"type": "response.created"}),
            json!({"type": "response.output_text.delta", "output_index": 0,
                   "content_index": 0, "delta": "Hello "}),
            json!({"type": "response.output_text.delta", "output_index": 0,
                   "content_index": 0, "delta": "world"}),
            json!({"type": "response.output_text.done", "output_index": 0, "content_index": 0}),
            json!({"type": "response.completed",
                   "response": {"usage": {"input_tokens": 7, "output_tokens": 2}}}),
        ])
    }

    #[test]
    fn streaming_request_reaches_upstream_translated_and_returns_anthropic_sse() {
        let server = FakeResponses::start(text_reply());
        let pipeline = pipeline(&server.base_url);
        let mut frames = Vec::new();
        pipeline
            .stream(
                br#"{"model":"claude-sonnet-5","max_tokens":32,"system":"be brief",
                     "messages":[{"role":"user","content":"hi"}],"stream":true}"#,
                "msg_x",
                &AtomicBool::new(false),
                &mut |frame| {
                    frames.push(frame.to_string());
                    Ok(())
                },
            )
            .unwrap();

        // What the fake actually received is the real assertion: a translation
        // regression must fail here, not merely change our own output.
        let sent = server.request();
        assert_eq!(sent["model"], DEFAULT_CODEX_MODEL);
        assert_eq!(sent["instructions"], "be brief");
        assert_eq!(sent["max_output_tokens"], 32);
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(sent["input"][0]["content"][0]["text"], "hi");

        let rendered = frames.concat();
        assert!(rendered.contains("event: message_start"));
        assert!(rendered.contains(r#""text":"Hello "#));
        assert!(rendered.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
    }

    /// A non-streaming request still streams upstream; the events are folded
    /// back into one Message.
    #[test]
    fn non_streaming_request_aggregates_into_one_message() {
        let server = FakeResponses::start(text_reply());
        let message = pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"hi"}],"stream":false}"#,
                "msg_agg",
                &AtomicBool::new(false),
            )
            .unwrap();

        assert_eq!(
            server.request()["stream"],
            true,
            "upstream is always streamed"
        );
        assert_eq!(
            message,
            json!({
                "id": "msg_agg",
                "type": "message",
                "role": "assistant",
                "model": DEFAULT_CODEX_MODEL,
                "content": [{"type": "text", "text": "Hello world"}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 7, "output_tokens": 2},
            })
        );
    }

    #[test]
    fn tool_calls_aggregate_with_parsed_input() {
        let server = FakeResponses::start(sse(&[
            json!({"type": "response.created"}),
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "call_id": "c1", "name": "weather"}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0,
                   "delta": "{\"city\":"}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0,
                   "delta": "\"Paris\"}"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "function_call", "call_id": "c1", "name": "weather"}}),
            json!({"type": "response.completed", "response": {"usage": {}}}),
        ]));
        let message = pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"weather?"}]}"#,
                "msg_tool",
                &AtomicBool::new(false),
            )
            .unwrap();

        assert_eq!(message["stop_reason"], "tool_use");
        assert_eq!(
            message["content"][0],
            json!({"type": "tool_use", "id": "c1", "name": "weather",
                   "input": {"city": "Paris"}})
        );
    }

    #[test]
    fn tool_definitions_and_choice_reach_upstream() {
        let server = FakeResponses::start(text_reply());
        pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"x"}],
                     "tools":[{"name":"lookup","input_schema":{"type":"object"}}],
                     "tool_choice":{"type":"any","disable_parallel_tool_use":true}}"#,
                "msg_tools",
                &AtomicBool::new(false),
            )
            .unwrap();
        let sent = server.request();
        assert_eq!(sent["tools"][0]["type"], "function");
        assert_eq!(sent["tools"][0]["name"], "lookup");
        assert_eq!(sent["tool_choice"], "required");
        assert_eq!(sent["parallel_tool_calls"], false);
    }

    #[test]
    fn an_unsupported_request_fails_before_any_upstream_call() {
        let server = FakeResponses::start(text_reply());
        let error = pipeline(&server.base_url)
            .stream(
                br#"{"messages":[{"role":"user","content":"x"}],"top_k":5}"#,
                "msg_bad",
                &AtomicBool::new(false),
                &mut |_| Ok(()),
            )
            .unwrap_err();

        assert_eq!(error.http_status(), 422);
        assert!(error.client_message().contains("top_k"));
        assert!(
            server.requests.lock().unwrap().is_empty(),
            "a rejected request must never reach upstream"
        );
    }

    #[test]
    fn malformed_json_is_a_400() {
        let server = FakeResponses::start(text_reply());
        let error = pipeline(&server.base_url)
            .complete(b"not json", "msg", &AtomicBool::new(false))
            .unwrap_err();
        assert_eq!(error.http_status(), 400);
    }

    #[test]
    fn missing_credentials_are_a_401_that_names_no_secret() {
        struct NoCredentials;
        impl CredentialSource for NoCredentials {
            fn resolve(&self) -> Result<UpstreamTarget, UpstreamError> {
                Err(UpstreamError::Credentials("OPENAI_API_KEY is not set"))
            }
        }
        let pipeline = Pipeline::new(UpstreamClient::new(
            NoCredentials,
            UpstreamConfig::default(),
        ));
        let error = pipeline
            .complete(
                br#"{"messages":[{"role":"user","content":"x"}]}"#,
                "msg",
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert_eq!(error.http_status(), 401);
        assert!(!error.client_message().contains("OPENAI_API_KEY"));
    }

    #[test]
    fn upstream_status_maps_to_a_downstream_status_without_the_body() {
        let body = r#"{"error":{"message":"key sk-secret for org org_1"}}"#;
        let reply = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes();
        let server = FakeResponses::start(reply);
        let error = pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"x"}]}"#,
                "msg",
                &AtomicBool::new(false),
            )
            .unwrap_err();

        assert_eq!(error.http_status(), 403);
        let rendered = format!("{} {:?}", error.client_message(), error);
        assert!(!rendered.contains("sk-secret"));
        assert!(!rendered.contains("org_1"));
    }

    /// A mid-stream failure cannot change the status, so the turn is closed
    /// in-band with a sanitized SSE error rather than left hanging.
    #[test]
    fn a_mid_stream_failure_emits_an_sse_error_after_the_partial_output() {
        let head = "event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"partial\"}\n\n";
        // Claims far more than it sends, then hangs up.
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 65535\r\nConnection: close\r\n\r\n{head}"
        )
        .into_bytes();
        let server = FakeResponses::start(reply);
        let mut frames = Vec::new();
        let error = pipeline(&server.base_url)
            .stream(
                br#"{"messages":[{"role":"user","content":"x"}],"stream":true}"#,
                "msg_mid",
                &AtomicBool::new(false),
                &mut |frame| {
                    frames.push(frame.to_string());
                    Ok(())
                },
            )
            .unwrap_err();

        assert!(matches!(error, PipelineError::Upstream(_)));
        let rendered = frames.concat();
        assert!(
            rendered.contains("partial"),
            "partial output should survive"
        );
        assert!(rendered.contains("event: content_block_stop"));
        assert!(rendered.contains("event: error"));
        assert!(rendered.contains("upstream provider error"));
    }

    #[test]
    fn aggregator_folds_multiple_blocks_and_reports_errors() {
        let mut aggregator = MessageAggregator::new();
        for frame in [
            r#"event: message_start
data: {"type":"message_start","message":{"id":"m1","model":"gpt","usage":{"input_tokens":3}}}

"#,
            r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

"#,
            r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a"}}

"#,
            r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"b"}}

"#,
            r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}

"#,
            r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}

"#,
        ] {
            aggregator.push_frame(frame);
        }
        assert!(!aggregator.errored());
        let message = aggregator.finish();
        assert_eq!(message["id"], "m1");
        assert_eq!(message["content"][0]["text"], "ab");
        assert_eq!(message["usage"]["input_tokens"], 3);
        assert_eq!(message["usage"]["output_tokens"], 9);

        let mut failed = MessageAggregator::new();
        failed.push_frame("event: error\ndata: {\"type\":\"error\"}\n\n");
        assert!(failed.errored());
    }

    /// Truncated tool arguments must not produce a block whose `input` is
    /// invalid JSON; an empty object is the safe reading.
    #[test]
    fn truncated_tool_arguments_aggregate_to_an_empty_object() {
        let mut aggregator = MessageAggregator::new();
        aggregator.push_frame(
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"c\",\"name\":\"n\",\"input\":{}}}\n\n",
        );
        aggregator.push_frame(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n",
        );
        let message = aggregator.finish();
        assert_eq!(message["content"][0]["input"], json!({}));
    }

    #[test]
    fn cancellation_propagates_out_of_the_pipeline() {
        let server = FakeResponses::start(text_reply());
        let error = pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"x"}]}"#,
                "msg",
                &AtomicBool::new(true),
            )
            .unwrap_err();
        assert_eq!(error, PipelineError::Upstream(UpstreamError::Cancelled));
    }
}
