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
use std::time::Duration;

use crate::codex_model::ModelSpec;
use crate::codex_sse::{FrameDecoder, InBandFailure, StreamTranslator};
use crate::codex_translate::{
    default_model_spec, translate_bytes, SystemPlacement, TranslateError, TranslateOptions,
};
use crate::codex_upstream::{
    CredentialSource, FailureClass, UpstreamClient, UpstreamError, UpstreamFailure,
    CLUD_CREDENTIALS_EXPIRED, CREDENTIALS_EXPIRED,
};

/// "Client Closed Request". Not in the RFC status registry, but the
/// conventional code for it and unambiguous in a log — and by the time it is
/// chosen there is normally no reader left to receive it anyway.
const CLIENT_CLOSED_REQUEST: u16 = 499;

/// What the client is told when the account is out of quota.
///
/// Names the condition and an action. The previous text, "upstream provider
/// returned status 429", named neither -- and read identically for an ordinary
/// throttle, which is how a drained account went unnoticed for hours.
const QUOTA_EXHAUSTED_MESSAGE: &str =
    "upstream account quota exhausted -- check your plan usage, or switch providers with --claude";

/// What a completed stream is worth reporting even though it succeeded at the
/// HTTP layer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamSummary {
    /// The turn ended in a drained account or dead credentials, delivered
    /// in-band. HTTP status is already committed to 200 by then, so this is
    /// the only way the caller can learn it happened.
    pub terminal_account_failure: bool,
    /// Safe upstream classification captured from an in-band 400. This is
    /// operator-only metadata; the raw error body never leaves the translator.
    pub in_band_failure: Option<InBandFailure>,
    /// Field names, item kinds, and counts from the translated Responses
    /// request. It intentionally omits all user/model/tool values.
    pub request_shape: serde_json::Value,
}

/// A provider failure delivered *inside* an otherwise-successful stream.
///
/// The ChatGPT backend commonly reports quota exhaustion this way: HTTP 200,
/// then a `response.failed` event. Both fields are ours -- the type comes from
/// classifying the upstream error object, the message is synthesized from that
/// type. No upstream byte is carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    Translate(TranslateError),
    Upstream(UpstreamError),
    /// An in-band provider failure, classified rather than flattened.
    Provider(ProviderFailure),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Translate(error) => write!(formatter, "{error}"),
            Self::Upstream(error) => write!(formatter, "{error}"),
            Self::Provider(failure) => write!(formatter, "{}", failure.message),
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
            // Translation is total (#750 A1), so the only translate failure
            // left is a request that is not a valid Messages request at all.
            Self::Translate(TranslateError::Invalid(_)) => 400,
            Self::Upstream(UpstreamError::Credentials(_)) => 401,
            Self::Upstream(UpstreamError::Status(failure)) => match failure.status() {
                400 | 401 | 403 | 404 | 413 | 422 | 429 => failure.status(),
                _ => 502,
            },
            // The classification decides the status, not the transport. A
            // quota failure that arrived inside a 200 is still a 429.
            Self::Provider(failure) => match failure.kind.as_str() {
                "billing_error" | "rate_limit_error" => 429,
                "invalid_request_error" => 400,
                "authentication_error" => 401,
                _ => 502,
            },
            Self::Upstream(UpstreamError::Timeout) => 504,
            // 502 means "the gateway hop failed", so only the failures that
            // really are gateway failures keep it (#764). The three below are
            // not: an oversized response is a payload problem, and a cancelled
            // or hung-up request has no reader left to serve.
            Self::Upstream(UpstreamError::TooLarge) => 413,
            Self::Upstream(UpstreamError::Cancelled) => CLIENT_CLOSED_REQUEST,
            Self::Upstream(UpstreamError::Downstream(_)) => CLIENT_CLOSED_REQUEST,
            Self::Upstream(UpstreamError::Transport(_)) => 502,
        }
    }

    /// The upstream classification, where the failure carries one.
    ///
    /// Exposed so the HTTP layer can choose an error *type* from what was
    /// classified rather than re-deriving it from a status code that has
    /// already lost the distinction.
    pub fn failure_class(&self) -> Option<FailureClass> {
        match self {
            Self::Upstream(error) => error.failure().map(UpstreamFailure::class),
            Self::Provider(failure) if failure.kind == "billing_error" => {
                Some(FailureClass::Exhausted)
            }
            Self::Translate(_) | Self::Provider(_) => None,
        }
    }

    /// Seconds to put in a `Retry-After` header, if upstream gave a hint.
    pub fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::Upstream(error) => error
                .failure()
                .and_then(|failure| failure.retry_after().or_else(|| failure.resets_in()))
                .map(|hint| hint.as_secs()),
            Self::Translate(_) | Self::Provider(_) => None,
        }
    }

    /// Whether this is a terminal account-level failure -- quota exhausted or
    /// credentials that will not self-heal. These are the two the user must
    /// act on personally; everything else is the bridge's problem.
    pub fn is_terminal_account_failure(&self) -> bool {
        matches!(self.failure_class(), Some(FailureClass::Exhausted))
            || matches!(self, Self::Upstream(UpstreamError::Credentials(_)))
            || matches!(self, Self::Provider(failure) if failure.kind == "authentication_error")
    }

    /// The classified upstream summary, for the operator log only.
    ///
    /// Never reaches the client: [`Self::client_message`] is what the harness
    /// sees, and it deliberately carries less.
    pub fn upstream_diagnostic(&self) -> Option<String> {
        match self {
            Self::Upstream(error) => error.failure().map(UpstreamFailure::diagnostic),
            Self::Provider(failure) => Some(format!("in-band provider error: {}", failure.kind)),
            Self::Translate(_) => None,
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
            // Already synthesized by the translator from the classification.
            Self::Provider(failure) => failure.message.clone(),
            // An expired login is the one credential failure with an action
            // attached, so it is worth forwarding verbatim. Every other reason
            // names an environment variable and stays behind the generic text.
            Self::Upstream(UpstreamError::Credentials(what))
                if *what == CREDENTIALS_EXPIRED || *what == CLUD_CREDENTIALS_EXPIRED =>
            {
                what.to_string()
            }
            Self::Upstream(UpstreamError::Credentials(_)) => {
                "the Codex bridge has no upstream credentials".to_string()
            }
            Self::Upstream(UpstreamError::Timeout) => "upstream request timed out".to_string(),
            // Only the request id crosses over. It is an opaque correlation
            // handle, and it is the difference between a bug report that can be
            // traced upstream and one that cannot. The body never travels: it
            // can carry account identifiers and key fragments.
            Self::Upstream(UpstreamError::Status(failure)) => {
                // Lead with what happened, not with a number. "status 429"
                // names no cause, no remedy, and reads identically for an
                // ordinary throttle and a drained account -- which is how a
                // real exhaustion went unnoticed until the balance was gone.
                let mut message = match failure.class() {
                    FailureClass::Exhausted => QUOTA_EXHAUSTED_MESSAGE.to_string(),
                    _ if failure.status() == 429 => {
                        "upstream rate limited this request".to_string()
                    }
                    _ => format!("upstream provider returned status {}", failure.status()),
                };
                // Prefer the server's own `Retry-After` over a body hint; it is
                // the value the retry loop honoured, so the two agree.
                if let Some(resets) = failure.retry_after().or_else(|| failure.resets_in()) {
                    message.push_str(&format!("; retry in {}", humanize(resets)));
                }
                if let Some(id) = failure.request_id() {
                    message.push_str(&format!(" (request-id {id})"));
                }
                message
            }
            Self::Upstream(UpstreamError::Transport(_)) => "upstream unreachable".to_string(),
            Self::Upstream(UpstreamError::TooLarge) => {
                "upstream response exceeded the size budget".to_string()
            }
            Self::Upstream(UpstreamError::Cancelled) => "request cancelled".to_string(),
            Self::Upstream(UpstreamError::Downstream(_)) => {
                "downstream client disconnected".to_string()
            }
        }
    }
}

/// Render a duration the way a person reads a clock, not a counter.
///
/// A quota reset is routinely days out, and `442242s` is a number a user has
/// to do arithmetic on before it means anything.
fn humanize(duration: Duration) -> String {
    let total = duration.as_secs();
    let (days, hours, minutes) = (
        total / 86_400,
        (total % 86_400) / 3_600,
        (total % 3_600) / 60,
    );
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{total}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
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
    provider_error: Option<ProviderFailure>,
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
            Some("error") => {
                self.errored = true;
                // Keep the classification the translator already computed.
                // Reducing it to a bool is what forced `complete` to relabel a
                // billing failure as a transport failure, which then became a
                // 502 `api_error` -- the signal erased by the code holding it.
                if let Some(error) = value.get("error") {
                    self.provider_error = Some(ProviderFailure {
                        kind: string_at(error, "type"),
                        message: string_at(error, "message"),
                    });
                }
            }
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

    pub fn provider_error(&self) -> Option<&ProviderFailure> {
        self.provider_error.as_ref()
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
    default_model: ModelSpec,
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
            default_model: default_model_spec(),
        }
    }

    /// Pin the selection used when a request carries no model of its own.
    /// This is how a launch-time `--model terra@high` reaches the wire.
    pub fn with_default_model(mut self, model: ModelSpec) -> Self {
        self.default_model = model;
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
    ) -> Result<StreamSummary, PipelineError> {
        let (upstream_body, model, tool_names, request_shape) = self.prepare(request_body)?;

        let mut decoder = FrameDecoder::new();
        let mut translator = StreamTranslator::new(model, message_id).with_tool_names(tool_names);
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
                Ok(StreamSummary {
                    terminal_account_failure: translator.terminal_account_failure(),
                    in_band_failure: translator.in_band_failure().cloned(),
                    request_shape,
                })
            }
            Err(error) => {
                // A failure after the first frame cannot change the status, so
                // it is reported in-band and the turn is closed cleanly rather
                // than left hanging.
                if translator.is_finished() {
                    return Err(PipelineError::Upstream(error));
                }
                for out in translator.fail_opaque() {
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
            // Propagate the classification, not a generic transport failure.
            // The old relabel turned every in-band provider error -- including
            // a drained account -- into `502 api_error`.
            return Err(match aggregator.provider_error() {
                Some(failure) => PipelineError::Provider(failure.clone()),
                None => PipelineError::Upstream(UpstreamError::Transport("upstream stream failed")),
            });
        }
        Ok(aggregator.finish())
    }

    /// Translate the request and force streaming upstream.
    #[allow(clippy::type_complexity)]
    fn prepare(
        &self,
        request_body: &[u8],
    ) -> Result<
        (
            Vec<u8>,
            String,
            std::collections::HashMap<String, String>,
            serde_json::Value,
        ),
        PipelineError,
    > {
        let target = self
            .client
            .credentials()
            .resolve()
            .map_err(PipelineError::Upstream)?;
        let options = TranslateOptions {
            model: target
                .model_override()
                .cloned()
                .unwrap_or_else(|| self.default_model.clone()),
            // The Codex backend expects `instructions` to be Codex's own
            // prompt, so a foreign system prompt travels as a developer
            // message there instead. See #750 A3.
            system_placement: if target.uses_codex_backend() {
                SystemPlacement::DeveloperMessage
            } else {
                SystemPlacement::Instructions
            },
            // Codex uses its session id here. Without a stable key every
            // turn re-pays full input price.
            prompt_cache_key: target
                .prompt_cache_key()
                .map(str::to_string)
                .or_else(|| Some(self.client.session_id().to_string())),
            service_tier: None,
        };
        let translated =
            translate_bytes(request_body, &options).map_err(PipelineError::Translate)?;
        let request_shape = translated_request_shape(&translated.request);
        let model = translated.request.model.clone();
        let body = serde_json::to_vec(&translated.request).map_err(|error| {
            PipelineError::Translate(TranslateError::Invalid(error.to_string()))
        })?;
        Ok((body, model, translated.tool_names, request_shape))
    }
}

fn translated_request_shape(
    request: &crate::codex_translate::ResponsesRequest,
) -> serde_json::Value {
    let input_kinds = request
        .input
        .iter()
        .map(|item| match item {
            crate::codex_translate::InputItem::Message { .. } => "message",
            crate::codex_translate::InputItem::FunctionCall { .. } => "function_call",
            crate::codex_translate::InputItem::FunctionCallOutput { .. } => "function_call_output",
            crate::codex_translate::InputItem::Reasoning { .. } => "reasoning",
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "top_level_fields": [
            "model", "input", "stream", "store", "include", "instructions", "tools",
            "tool_choice", "parallel_tool_calls", "reasoning", "prompt_cache_key", "service_tier"
        ],
        "input_count": request.input.len(),
        "input_kinds": input_kinds,
        "tool_count": request.tools.as_ref().map_or(0, Vec::len),
        "has_instructions": request.instructions.is_some(),
        "has_tool_choice": request.tool_choice.is_some(),
        "has_reasoning": request.reasoning.is_some(),
        "has_prompt_cache_key": request.prompt_cache_key.is_some(),
        "has_service_tier": request.service_tier.is_some(),
    })
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
        assert_eq!(sent["model"], "gpt-5.6-terra");
        assert_eq!(sent["instructions"], "be brief");
        assert!(sent.get("max_output_tokens").is_none());
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
                "model": "gpt-5.6-terra",
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

    /// #750 A1: translation is total, so a request carrying Anthropic-only
    /// fields still reaches upstream -- with those fields dropped rather than
    /// turned into a 4xx the user cannot act on.
    #[test]
    fn anthropic_only_fields_are_dropped_and_the_request_still_flows() {
        let server = FakeResponses::start(text_reply());
        pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"x"}],"top_k":5,
                     "stop_sequences":["STOP"],"temperature":0.7,"max_tokens":16}"#,
                "msg_total",
                &AtomicBool::new(false),
            )
            .expect("a droppable field must not fail the request");

        let sent = server.request();
        for banned in [
            "top_k",
            "stop_sequences",
            "temperature",
            "max_output_tokens",
        ] {
            assert!(sent.get(banned).is_none(), "{banned} must not be forwarded");
        }
    }

    /// Conformance defaults must survive the whole pipeline, not just the
    /// translator unit tests.
    #[test]
    fn conformance_defaults_reach_upstream() {
        let server = FakeResponses::start(text_reply());
        pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"x"}]}"#,
                "msg_conf",
                &AtomicBool::new(false),
            )
            .unwrap();
        let sent = server.request();
        assert_eq!(sent["store"], false);
        assert_eq!(sent["include"][0], "reasoning.encrypted_content");
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["reasoning"]["effort"], "medium");
        assert_eq!(sent["parallel_tool_calls"], true);
        // A stable cache key is what buys prompt-cache hits across turns.
        assert!(sent["prompt_cache_key"]
            .as_str()
            .is_some_and(|k| !k.is_empty()));
    }

    /// The billed default, asserted as a literal on the wire rather than
    /// through `DEFAULT_CODEX_MODEL` — a constant-based assertion follows the
    /// constant wherever it goes and cannot notice a change in what the user
    /// is charged for. `terra` at `medium` is the pair #776 selected: `sol`
    /// costs 2.5x on both input and output, and `medium` is terra's own
    /// catalog default effort.
    #[test]
    fn the_billed_default_is_terra_at_medium() {
        let server = FakeResponses::start(text_reply());
        pipeline(&server.base_url)
            .complete(
                br#"{"messages":[{"role":"user","content":"x"}]}"#,
                "msg_default",
                &AtomicBool::new(false),
            )
            .unwrap();
        let sent = server.request();
        assert_eq!(sent["model"], "gpt-5.6-terra");
        assert_eq!(sent["reasoning"]["effort"], "medium");
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
        // The diagnostic is richer than the client message, and is still clean:
        // it is the only place the body is allowed to have left a trace.
        let diagnostic = error.upstream_diagnostic().expect("a status diagnostic");
        assert!(diagnostic.contains("status=403"), "{diagnostic}");
        assert!(!diagnostic.contains("sk-secret"), "{diagnostic}");
        assert!(!diagnostic.contains("org_1"), "{diagnostic}");
    }

    /// 502 means "the gateway hop failed". Failures that are not that must not
    /// borrow it, or every one of them becomes indistinguishable in a log.
    #[test]
    fn non_gateway_failures_no_longer_report_502() {
        let cases = [
            (UpstreamError::TooLarge, 413_u16),
            (UpstreamError::Cancelled, 499),
            (UpstreamError::Downstream("client hung up"), 499),
            (UpstreamError::Timeout, 504),
            // These two genuinely are gateway failures and keep 502.
            (UpstreamError::Transport("connection failed"), 502),
        ];
        for (error, expected) in cases {
            let rendered = PipelineError::Upstream(error.clone());
            assert_eq!(rendered.http_status(), expected, "{error:?}");
            assert_ne!(
                rendered.client_message(),
                "upstream provider error",
                "{error:?} still reports the old catch-all message"
            );
        }
    }

    /// An unmapped upstream 5xx is still a 502 — that part was always right.
    #[test]
    fn an_unmapped_upstream_status_is_still_a_502() {
        let failure = UpstreamFailure::from_parts(503, |_| None, "overloaded", Duration::ZERO);
        let error = PipelineError::Upstream(UpstreamError::Status(failure));
        assert_eq!(error.http_status(), 502);
    }

    /// The reported failure, end to end at the message layer: the class is
    /// named, and the reset is rendered as a clock rather than a raw count of
    /// seconds a user has to do arithmetic on.
    #[test]
    fn an_exhausted_account_says_so_and_renders_the_reset_readably() {
        let body = r#"{"error":{"code":"usage_limit_reached","resets_in_seconds":442242}}"#;
        let failure = UpstreamFailure::from_parts(429, |_| None, body, Duration::ZERO);
        let error = PipelineError::Upstream(UpstreamError::Status(failure));
        assert_eq!(error.http_status(), 429);
        assert_eq!(error.failure_class(), Some(FailureClass::Exhausted));
        assert!(error.is_terminal_account_failure());

        let message = error.client_message();
        assert!(message.contains("quota exhausted"), "{message}");
        // 442242s -- the number the original report showed the user.
        assert!(message.contains("5d 2h"), "{message}");
        assert!(!message.contains("442242"), "{message}");
        assert_eq!(error.retry_after_seconds(), Some(442_242));
    }

    /// An ordinary throttle is not an exhaustion and keeps its own wording.
    #[test]
    fn a_plain_429_is_a_rate_limit_not_a_billing_failure() {
        let failure = UpstreamFailure::from_parts(429, |_| None, "slow down", Duration::ZERO);
        let error = PipelineError::Upstream(UpstreamError::Status(failure));
        assert_eq!(error.failure_class(), Some(FailureClass::Transient));
        assert!(!error.is_terminal_account_failure());
        assert!(error.client_message().contains("rate limited"));
    }

    #[test]
    fn humanize_reads_like_a_clock() {
        assert_eq!(humanize(Duration::from_secs(45)), "45s");
        assert_eq!(humanize(Duration::from_secs(600)), "10m");
        assert_eq!(humanize(Duration::from_secs(3_600)), "1h 0m");
        assert_eq!(humanize(Duration::from_secs(442_242)), "5d 2h");
    }

    /// The request id is an opaque correlation handle, and it is the difference
    /// between a report that can be traced upstream and one that cannot.
    #[test]
    fn the_request_id_reaches_the_client_message_but_the_body_does_not() {
        let body = r#"{"error":{"message":"key sk-secret for org org_1"}}"#;
        let failure = UpstreamFailure::from_parts(
            502,
            |name| (name == "x-request-id").then(|| "req_xyz".to_string()),
            body,
            Duration::ZERO,
        );
        let error = PipelineError::Upstream(UpstreamError::Status(failure));
        let message = error.client_message();
        assert!(message.contains("req_xyz"), "{message}");
        assert!(!message.contains("sk-secret"), "{message}");
        assert!(!message.contains("org_1"), "{message}");
    }

    #[test]
    fn an_expired_login_is_the_one_credential_reason_forwarded_verbatim() {
        let expired = PipelineError::Upstream(UpstreamError::Credentials(CREDENTIALS_EXPIRED));
        assert_eq!(expired.http_status(), 401);
        assert!(expired.client_message().contains("codex login"));

        // Every other reason stays generic: they name environment variables.
        let missing =
            PipelineError::Upstream(UpstreamError::Credentials("OPENAI_API_KEY is not set"));
        assert!(!missing.client_message().contains("OPENAI_API_KEY"));
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
