//! OpenAI Responses SSE -> Anthropic Messages SSE translation
//! (issue #627 phase 3, step 3).
//!
//! Two layers, deliberately separable so each can be tested without the other:
//!
//! 1. [`FrameDecoder`] is byte-level. It tolerates arbitrary network
//!    fragmentation (including one byte at a time), several frames in one
//!    read, CRLF or LF or bare CR, comment/heartbeat lines, and a final frame
//!    with no trailing blank line.
//! 2. [`StreamTranslator`] is semantic. It maps Responses events onto a valid
//!    Anthropic event sequence with monotonic content-block indices.
//!
//! The invariant that shapes the whole design: **never emit a malformed
//! Anthropic block.** A `content_block_start` for a tool call cannot be sent
//! until the call's id and name are both known, so argument deltas that arrive
//! first are buffered rather than forwarded into a block that does not exist
//! yet. Downstream, a half-formed tool block is worse than a late one -- it
//! makes the client fail to parse a turn it could otherwise have used.
//!
//! Nothing here talks to the network or sees credentials; step 5 wires it into
//! [`crate::codex_bridge`].

use std::collections::HashMap;

/// One decoded SSE frame. `event` is absent when the producer sent only data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

/// Byte-level SSE framing.
///
/// Line terminators are normalised to `\n` on ingest (the SSE spec treats
/// CRLF, LF, and a bare CR alike). A `\r` at the very end of a read is held
/// back until the next one so a CRLF split across two TCP segments is not
/// mistaken for a bare CR followed by a blank line.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: String,
    pending_cr: bool,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; return every frame that is now complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        let text = String::from_utf8_lossy(bytes);
        self.absorb(&text, false);
        self.drain(false)
    }

    /// Flush at end of stream. A producer that closes without a trailing blank
    /// line still has one frame's worth of buffered data, and dropping it would
    /// silently truncate the turn.
    pub fn finish(&mut self) -> Vec<SseFrame> {
        if self.pending_cr {
            self.buffer.push('\n');
            self.pending_cr = false;
        }
        self.drain(true)
    }

    fn absorb(&mut self, text: &str, _final_chunk: bool) {
        for character in text.chars() {
            match character {
                '\r' => {
                    if self.pending_cr {
                        // Two CRs in a row: the first was a line on its own.
                        self.buffer.push('\n');
                    }
                    self.pending_cr = true;
                }
                '\n' => {
                    // Either the LF of a CRLF pair or a bare LF; one newline
                    // either way.
                    self.pending_cr = false;
                    self.buffer.push('\n');
                }
                other => {
                    if self.pending_cr {
                        self.buffer.push('\n');
                        self.pending_cr = false;
                    }
                    self.buffer.push(other);
                }
            }
        }
    }

    fn drain(&mut self, flush: bool) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        while let Some(position) = self.buffer.find("\n\n") {
            let raw: String = self.buffer.drain(..position + 2).collect();
            if let Some(frame) = parse_frame(&raw) {
                frames.push(frame);
            }
        }
        if flush {
            let remainder: String = std::mem::take(&mut self.buffer);
            if let Some(frame) = parse_frame(&remainder) {
                frames.push(frame);
            }
        }
        frames
    }
}

fn parse_frame(raw: &str) -> Option<SseFrame> {
    let mut event = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for line in raw.split('\n') {
        if line.is_empty() {
            continue;
        }
        // A line beginning with a colon is a comment. Heartbeats arrive this
        // way and must not be mistaken for data.
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data_lines.push(value),
            _ => {}
        }
    }
    if event.is_none() && data_lines.is_empty() {
        return None;
    }
    Some(SseFrame {
        event,
        data: data_lines.join("\n"),
    })
}

// ---------------------------------------------------------------------------
// Semantic translation
// ---------------------------------------------------------------------------

/// One in-flight upstream function call.
#[derive(Debug, Default)]
struct CallState {
    index: Option<u32>,
    call_id: Option<String>,
    name: Option<String>,
    /// Everything received so far, whether buffered or already emitted.
    arguments: String,
    /// How much of `arguments` has been forwarded downstream.
    emitted: usize,
    closed: bool,
}

/// The thinking block currently being assembled from one reasoning item.
#[derive(Debug)]
struct ThinkingState {
    index: u32,
    parts: usize,
    signature: Option<String>,
}

/// Responses events -> Anthropic events.
///
/// Emits complete SSE frames as strings so the caller can write and flush each
/// one, which is what makes a turn render progressively.
#[derive(Debug)]
pub struct StreamTranslator {
    model: String,
    message_id: String,
    started: bool,
    finished: bool,
    next_index: u32,
    text_indices: HashMap<(i64, i64), u32>,
    open_blocks: Vec<u32>,
    calls: Vec<CallState>,
    /// Upstream events do not all carry the same identifier, so a call is
    /// reachable by output index, call id, or item id.
    aliases: HashMap<String, usize>,
    thinking: Option<ThinkingState>,
    saw_tool_call: bool,
    has_text_delta: bool,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    stop_reason: Option<String>,
    /// Shortened tool name -> original, so the client sees what it sent.
    tool_names: HashMap<String, String>,
    /// Set when the stream ended in a drained account or dead credentials --
    /// the two failures a user must act on personally.
    terminal_account_failure: bool,
}

impl StreamTranslator {
    pub fn new(model: impl Into<String>, message_id: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            message_id: message_id.into(),
            started: false,
            finished: false,
            next_index: 0,
            text_indices: HashMap::new(),
            open_blocks: Vec::new(),
            calls: Vec::new(),
            aliases: HashMap::new(),
            thinking: None,
            saw_tool_call: false,
            has_text_delta: false,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            stop_reason: None,
            tool_names: HashMap::new(),
            terminal_account_failure: false,
        }
    }

    /// Supply the shortening map produced by the request translator.
    pub fn with_tool_names(mut self, tool_names: HashMap<String, String>) -> Self {
        self.tool_names = tool_names;
        self
    }

    /// Translate one upstream frame into zero or more Anthropic frames.
    pub fn push(&mut self, frame: &SseFrame) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let value: serde_json::Value = match serde_json::from_str(&frame.data) {
            Ok(value) => value,
            // `[DONE]` sentinels and other non-JSON payloads carry nothing to
            // translate; they are not errors.
            Err(_) => return Vec::new(),
        };
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .or(frame.event.as_deref())
            .unwrap_or("")
            .to_string();

        let mut out = Vec::new();
        match kind.as_str() {
            "response.created" | "response.in_progress" => self.ensure_started(&mut out),

            "response.output_text.delta" => {
                self.ensure_started(&mut out);
                self.finalize_thinking(&mut out);
                self.has_text_delta = true;
                let key = text_key(&value);
                let index = self.ensure_text_block(key, &mut out);
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    out.push(text_delta_frame(index, delta));
                }
            }
            "response.content_part.done" | "response.output_text.done" => {
                if let Some(index) = self.text_indices.get(&text_key(&value)).copied() {
                    self.close_block(index, &mut out);
                }
            }

            // --- reasoning -------------------------------------------------
            "response.reasoning_summary_part.added" => {
                self.ensure_started(&mut out);
                if let Some(state) = self.thinking.as_mut() {
                    // Several summary parts belong to ONE thinking block; only
                    // the item's `done` carries the signature that closes it.
                    if state.parts > 0 {
                        let index = state.index;
                        state.parts += 1;
                        out.push(thinking_delta_frame(index, "\n\n"));
                    } else {
                        state.parts += 1;
                    }
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.ensure_started(&mut out);
                let index = self.ensure_thinking_block(&mut out);
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    out.push(thinking_delta_frame(index, delta));
                }
            }

            "response.output_item.added" => {
                self.ensure_started(&mut out);
                let Some(item) = value.get("item") else {
                    return out;
                };
                match item.get("type").and_then(serde_json::Value::as_str) {
                    Some("function_call") => {
                        self.finalize_thinking(&mut out);
                        let slot = self.slot_for(&value, item);
                        merge_identity(&mut self.calls[slot], item);
                        if let Some(seed) =
                            item.get("arguments").and_then(serde_json::Value::as_str)
                        {
                            self.calls[slot].arguments.push_str(seed);
                        }
                        self.saw_tool_call = true;
                        self.open_call(slot, &mut out);
                    }
                    Some("reasoning") => {
                        // Close any previous item so an unterminated block
                        // cannot leak into this one.
                        self.finalize_thinking(&mut out);
                    }
                    _ => {}
                }
            }

            "response.function_call_arguments.delta" => {
                self.ensure_started(&mut out);
                self.finalize_thinking(&mut out);
                let slot = self.slot_for(&value, &serde_json::Value::Null);
                self.saw_tool_call = true;
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    self.calls[slot].arguments.push_str(delta);
                }
                self.flush_call_arguments(slot, &mut out);
            }
            "response.function_call_arguments.done" => {
                let slot = self.slot_for(&value, &serde_json::Value::Null);
                if let Some(arguments) = value.get("arguments").and_then(serde_json::Value::as_str)
                {
                    // The terminal value is absolute. Adopt it only when it
                    // extends what we already have, so a stream that sent both
                    // deltas and a final value cannot double-count.
                    if arguments.starts_with(self.calls[slot].arguments.as_str()) {
                        self.calls[slot].arguments = arguments.to_string();
                    }
                }
                self.flush_call_arguments(slot, &mut out);
            }

            "response.output_item.done" => {
                let Some(item) = value.get("item") else {
                    return out;
                };
                match item.get("type").and_then(serde_json::Value::as_str) {
                    Some("function_call") => {
                        let slot = self.slot_for(&value, item);
                        merge_identity(&mut self.calls[slot], item);
                        if let Some(arguments) =
                            item.get("arguments").and_then(serde_json::Value::as_str)
                        {
                            if arguments.starts_with(self.calls[slot].arguments.as_str()) {
                                self.calls[slot].arguments = arguments.to_string();
                            }
                        }
                        self.saw_tool_call = true;
                        self.open_call(slot, &mut out);
                        self.flush_call_arguments(slot, &mut out);
                        if let Some(index) = self.calls[slot].index {
                            self.close_block(index, &mut out);
                            self.calls[slot].closed = true;
                        }
                    }
                    Some("reasoning") => {
                        let signature = item
                            .get("encrypted_content")
                            .and_then(serde_json::Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string);
                        if signature.is_some() && self.thinking.is_none() {
                            // A signature with no summary text still has to
                            // reach the client: it is the round-trip carrier.
                            self.ensure_thinking_block(&mut out);
                        }
                        if let Some(state) = self.thinking.as_mut() {
                            state.signature = signature;
                        }
                        self.finalize_thinking(&mut out);
                    }
                    Some("message") => {
                        // Fallback for a stream that never sent text deltas.
                        if !self.has_text_delta {
                            if let Some(text) = message_item_text(item) {
                                let index = self.ensure_text_block((0, 0), &mut out);
                                out.push(text_delta_frame(index, &text));
                                self.close_block(index, &mut out);
                            }
                        }
                    }
                    _ => {}
                }
            }

            "response.completed" | "response.incomplete" => {
                self.absorb_usage(&value);
                if self.stop_reason.is_none() {
                    self.stop_reason = Some(map_stop_reason(&kind, &value, self.saw_tool_call));
                }
                out.extend(self.finish());
            }
            "response.failed" | "error" => out.extend(self.fail(&value)),
            _ => {}
        }
        out
    }

    /// Close the turn: shut any open blocks, then message_delta/stop.
    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        self.finalize_thinking(&mut out);
        for index in std::mem::take(&mut self.open_blocks) {
            out.push(content_block_stop_frame(index));
        }
        let stop_reason = self.stop_reason.clone().unwrap_or_else(|| {
            if self.saw_tool_call {
                "tool_use".to_string()
            } else {
                "end_turn".to_string()
            }
        });
        // Anthropic's input_tokens excludes cached tokens; the Responses usage
        // includes them. Subtracting keeps clients from double-counting.
        let input_tokens = self.input_tokens.saturating_sub(self.cached_tokens);
        let mut usage = serde_json::json!({
            "input_tokens": input_tokens,
            "output_tokens": self.output_tokens,
        });
        if self.cached_tokens > 0 {
            usage["cache_read_input_tokens"] = serde_json::json!(self.cached_tokens);
        }
        out.push(anthropic_frame(
            "message_delta",
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": usage,
            }),
        ));
        out.push(anthropic_frame(
            "message_stop",
            serde_json::json!({"type": "message_stop"}),
        ));
        self.finished = true;
        out
    }

    /// Terminate with a sanitized error.
    ///
    /// The upstream body is never echoed: it can carry account identifiers and
    /// key fragments, and this frame is written straight to the harness.
    pub fn fail(&mut self, upstream: &serde_json::Value) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.finalize_thinking(&mut out);
        for index in std::mem::take(&mut self.open_blocks) {
            out.push(content_block_stop_frame(index));
        }
        let kind = upstream_error_type(upstream);
        if matches!(kind, "billing_error" | "authentication_error") {
            self.terminal_account_failure = true;
        }
        out.push(anthropic_frame(
            "error",
            serde_json::json!({
                "type": "error",
                "error": {
                    "type": kind,
                    // Synthesized from the classification, never echoed. The
                    // upstream body can quote account identifiers and key
                    // fragments, so it is read to classify and then dropped
                    // (#630). What was wrong before was not the redaction --
                    // it was that the body was never *read*, so every failure
                    // collapsed onto one constant and an out-of-credits
                    // condition could not be reported as one.
                    "message": upstream_error_message(kind),
                },
            }),
        ));
        self.finished = true;
        out
    }

    /// Whether the stream ended in a failure the user must act on personally
    /// -- a drained account or dead credentials. The bridge uses this to
    /// surface a quota failure that arrived *inside* a 200 SSE stream, which
    /// otherwise produces HTTP 200 and no diagnostic anywhere.
    pub fn terminal_account_failure(&self) -> bool {
        self.terminal_account_failure
    }

    /// Terminate with a sanitized error and no upstream detail.
    pub fn fail_opaque(&mut self) -> Vec<String> {
        self.fail(&serde_json::Value::Null)
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn ensure_started(&mut self, out: &mut Vec<String>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(anthropic_frame(
            "message_start",
            serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                },
            }),
        ));
    }

    fn ensure_text_block(&mut self, key: (i64, i64), out: &mut Vec<String>) -> u32 {
        if let Some(index) = self.text_indices.get(&key) {
            return *index;
        }
        let index = self.allocate_index();
        self.text_indices.insert(key, index);
        self.open_blocks.push(index);
        out.push(anthropic_frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "text", "text": ""},
            }),
        ));
        index
    }

    fn ensure_thinking_block(&mut self, out: &mut Vec<String>) -> u32 {
        if let Some(state) = self.thinking.as_ref() {
            return state.index;
        }
        let index = self.allocate_index();
        self.open_blocks.push(index);
        self.thinking = Some(ThinkingState {
            index,
            parts: 1,
            signature: None,
        });
        out.push(anthropic_frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "thinking", "thinking": ""},
            }),
        ));
        index
    }

    /// Close the open thinking block, emitting its signature exactly once.
    ///
    /// The signature is the reasoning item's `encrypted_content` verbatim --
    /// that is what the client echoes back so the next turn can carry the
    /// reasoning forward under `store: false`.
    fn finalize_thinking(&mut self, out: &mut Vec<String>) {
        let Some(state) = self.thinking.take() else {
            return;
        };
        if let Some(signature) = state.signature.as_deref() {
            out.push(anthropic_frame(
                "content_block_delta",
                serde_json::json!({
                    "type": "content_block_delta",
                    "index": state.index,
                    "delta": {"type": "signature_delta", "signature": signature},
                }),
            ));
        }
        self.close_block(state.index, out);
    }

    /// Resolve the slot for a call, registering every identifier the event
    /// carries so a later event can find it by any of them.
    fn slot_for(&mut self, event: &serde_json::Value, item: &serde_json::Value) -> usize {
        let mut keys = Vec::new();
        if let Some(index) = event
            .get("output_index")
            .and_then(serde_json::Value::as_i64)
        {
            keys.push(format!("output:{index}"));
        }
        for source in [event, item] {
            for field in ["call_id", "item_id", "id"] {
                if let Some(value) = source.get(field).and_then(serde_json::Value::as_str) {
                    if !value.is_empty() {
                        let prefix = if field == "call_id" { "call" } else { "item" };
                        keys.push(format!("{prefix}:{value}"));
                    }
                }
            }
        }
        let slot = keys
            .iter()
            .find_map(|key| self.aliases.get(key).copied())
            .unwrap_or_else(|| {
                self.calls.push(CallState::default());
                self.calls.len() - 1
            });
        for key in keys {
            self.aliases.insert(key, slot);
        }
        slot
    }

    /// Open a tool block once -- and only once -- its id and name are known.
    fn open_call(&mut self, slot: usize, out: &mut Vec<String>) {
        let call = &self.calls[slot];
        if call.index.is_some() || call.closed {
            return;
        }
        let (Some(call_id), Some(name)) = (call.call_id.clone(), call.name.clone()) else {
            return;
        };
        // Restore the client's original tool name if we shortened it.
        let name = self.tool_names.get(&name).cloned().unwrap_or(name);
        let index = self.allocate_index();
        self.calls[slot].index = Some(index);
        self.open_blocks.push(index);
        out.push(anthropic_frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": sanitize_tool_id(&call_id),
                    "name": name,
                    "input": {},
                },
            }),
        ));
    }

    /// Forward whatever argument text has not been sent yet.
    fn flush_call_arguments(&mut self, slot: usize, out: &mut Vec<String>) {
        let Some(index) = self.calls[slot].index else {
            return;
        };
        let call = &mut self.calls[slot];
        if call.emitted >= call.arguments.len() {
            return;
        }
        let pending = call.arguments[call.emitted..].to_string();
        call.emitted = call.arguments.len();
        out.push(anthropic_frame(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "input_json_delta", "partial_json": pending},
            }),
        ));
    }

    fn close_block(&mut self, index: u32, out: &mut Vec<String>) {
        if let Some(position) = self.open_blocks.iter().position(|open| *open == index) {
            self.open_blocks.remove(position);
            out.push(content_block_stop_frame(index));
        }
    }

    fn allocate_index(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    fn absorb_usage(&mut self, value: &serde_json::Value) {
        let Some(usage) = value.pointer("/response/usage") else {
            return;
        };
        if let Some(input) = usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.input_tokens = input;
        }
        if let Some(output) = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.output_tokens = output;
        }
        if let Some(cached) = usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.cached_tokens = cached;
        }
    }
}

fn text_key(value: &serde_json::Value) -> (i64, i64) {
    (
        value
            .get("output_index")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        value
            .get("content_index")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
    )
}

fn message_item_text(item: &serde_json::Value) -> Option<String> {
    let parts = item.get("content")?.as_array()?;
    let text: String = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

/// Anthropic clients enforce `^[a-zA-Z0-9_-]+$` on `tool_use.id`; upstream
/// call ids do not always satisfy it.
fn sanitize_tool_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "tool_call".to_string()
    } else {
        sanitized
    }
}

fn map_stop_reason(kind: &str, value: &serde_json::Value, saw_tool_call: bool) -> String {
    // A turn that produced a tool call is a tool_use turn regardless of what
    // the transport called it.
    if saw_tool_call {
        return "tool_use".to_string();
    }
    let raw = value
        .pointer("/response/stop_reason")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/response/incomplete_details/reason")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("");
    if raw.is_empty() && kind == "response.incomplete" {
        // An incomplete response with no stated reason is, in practice, a
        // length cutoff.
        return "max_tokens".to_string();
    }
    match raw {
        "max_tokens" | "max_output_tokens" => "max_tokens",
        "content_filter" => "refusal",
        "end_turn"
        | "stop_sequence"
        | "pause_turn"
        | "refusal"
        | "model_context_window_exceeded" => raw,
        // "", "stop", "completed", "tool_use", "tool_calls", "function_call"
        // and anything unrecognised.
        _ => "end_turn",
    }
    .to_string()
}

fn upstream_error_type(value: &serde_json::Value) -> &'static str {
    let code = value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("code"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let kind = value
        .pointer("/response/error/type")
        .or_else(|| value.pointer("/error/type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match (code, kind) {
        ("cyber_policy", _) | (_, "invalid_request") => "invalid_request_error",
        ("rate_limit_exceeded", _) => "rate_limit_error",
        // `usage_limit_reached` is how the ChatGPT backend spells an
        // exhausted plan; it is a billing condition, not a throttle.
        ("insufficient_quota", _) | ("usage_limit_reached", _) | ("quota_exceeded", _) => {
            "billing_error"
        }
        ("context_length_exceeded", _) => "invalid_request_error",
        _ => "api_error",
    }
}

/// A safe, actionable message per error class.
///
/// Derived, never echoed: every string here is one we wrote.
fn upstream_error_message(kind: &str) -> &'static str {
    match kind {
        "billing_error" => {
            "upstream account quota exhausted -- check your plan usage, or switch providers with --claude"
        }
        "rate_limit_error" => "upstream rate limited this request",
        "invalid_request_error" => "upstream rejected the request as invalid",
        "authentication_error" => "upstream rejected the bridge's credentials",
        _ => "upstream provider error",
    }
}

fn merge_identity(call: &mut CallState, item: &serde_json::Value) {
    // `call_id` is the identifier a follow-up tool_result must quote; `id` is
    // the item's own handle. Prefer the former, accept the latter.
    if call.call_id.is_none() {
        if let Some(call_id) = item
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| item.get("id").and_then(serde_json::Value::as_str))
        {
            if !call_id.is_empty() {
                call.call_id = Some(call_id.to_string());
            }
        }
    }
    if call.name.is_none() {
        if let Some(name) = item.get("name").and_then(serde_json::Value::as_str) {
            if !name.is_empty() {
                call.name = Some(name.to_string());
            }
        }
    }
}

fn text_delta_frame(index: u32, text: &str) -> String {
    anthropic_frame(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "text_delta", "text": text},
        }),
    )
}

fn thinking_delta_frame(index: u32, text: &str) -> String {
    anthropic_frame(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "thinking_delta", "thinking": text},
        }),
    )
}

fn content_block_stop_frame(index: u32) -> String {
    anthropic_frame(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": index}),
    )
}

fn anthropic_frame(event: &str, body: serde_json::Value) -> String {
    format!("event: {event}\ndata: {body}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upstream(kind: &str, mut body: serde_json::Value) -> String {
        body["type"] = json!(kind);
        format!("event: {kind}\ndata: {body}\n\n")
    }

    /// Drive a whole upstream stream through both layers, splitting the input
    /// into `chunk` byte pieces to exercise fragmentation.
    fn run(stream: &str, chunk: usize) -> Vec<String> {
        let mut decoder = FrameDecoder::new();
        let mut translator = StreamTranslator::new("gpt-5.6-sol", "msg_test");
        let mut out = Vec::new();
        let bytes = stream.as_bytes();
        for piece in bytes.chunks(chunk.max(1)) {
            for frame in decoder.push(piece) {
                out.extend(translator.push(&frame));
            }
        }
        for frame in decoder.finish() {
            out.extend(translator.push(&frame));
        }
        out.extend(translator.finish());
        out
    }

    fn events(frames: &[String]) -> Vec<String> {
        frames
            .iter()
            .map(|frame| {
                frame
                    .lines()
                    .next()
                    .unwrap()
                    .trim_start_matches("event: ")
                    .to_string()
            })
            .collect()
    }

    fn bodies(frames: &[String]) -> Vec<serde_json::Value> {
        frames
            .iter()
            .map(|frame| {
                let data = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .expect("data line");
                serde_json::from_str(data).expect("valid JSON body")
            })
            .collect()
    }

    fn text_stream() -> String {
        [
            upstream("response.created", json!({})),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "Hel"}),
            ),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "lo"}),
            ),
            upstream(
                "response.output_text.done",
                json!({"output_index": 0, "content_index": 0}),
            ),
            upstream(
                "response.completed",
                json!({"response": {"usage": {"input_tokens": 11, "output_tokens": 2}}}),
            ),
        ]
        .concat()
    }

    #[test]
    fn text_stream_produces_a_balanced_anthropic_sequence() {
        let frames = run(&text_stream(), 4096);
        assert_eq!(
            events(&frames),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let bodies = bodies(&frames);
        assert_eq!(bodies[2]["delta"]["text"], "Hel");
        assert_eq!(bodies[3]["delta"]["text"], "lo");
        assert_eq!(bodies[5]["delta"]["stop_reason"], "end_turn");
        assert_eq!(bodies[5]["usage"]["output_tokens"], 2);
    }

    /// The core robustness property: the output must not depend on how the
    /// network split the input. Byte-at-a-time is the worst case.
    #[test]
    fn output_is_identical_at_every_fragmentation_boundary() {
        let stream = text_stream();
        let reference = run(&stream, stream.len());
        for chunk in [1, 2, 3, 5, 7, 13, 64, 512] {
            assert_eq!(
                run(&stream, chunk),
                reference,
                "fragmentation at {chunk} bytes changed the output"
            );
        }
    }

    #[test]
    fn crlf_comments_and_a_missing_final_blank_line_are_tolerated() {
        // CRLF throughout, a heartbeat comment, and no trailing blank line.
        let stream = concat!(
            ": keep-alive\r\n\r\n",
            "event: response.created\r\ndata: {\"type\":\"response.created\"}\r\n\r\n",
            ": another heartbeat\r\n\r\n",
            "event: response.output_text.delta\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,",
            "\"content_index\":0,\"delta\":\"hi\"}\r\n\r\n",
            "event: response.completed\r\n",
            "data: {\"type\":\"response.completed\"}"
        );
        for chunk in [1, 3, 4096] {
            let frames = run(stream, chunk);
            assert_eq!(
                events(&frames),
                vec![
                    "message_start",
                    "content_block_start",
                    "content_block_delta",
                    "content_block_stop",
                    "message_delta",
                    "message_stop",
                ],
                "chunk {chunk}"
            );
            assert_eq!(bodies(&frames)[2]["delta"]["text"], "hi");
        }
    }

    #[test]
    fn multiline_data_and_bare_cr_are_joined_per_spec() {
        let mut decoder = FrameDecoder::new();
        let frames = decoder.push(b"event: x\ndata: one\ndata: two\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                event: Some("x".into()),
                data: "one\ntwo".into()
            }]
        );

        // Bare CR is a line terminator too. Note the frame does not appear
        // until the stream is flushed: a trailing `\r` is indistinguishable
        // from the first half of a CRLF, so the decoder must hold it.
        let mut bare = FrameDecoder::new();
        assert!(bare.push(b"event: y\rdata: v\r\r").is_empty());
        assert_eq!(
            bare.finish(),
            vec![SseFrame {
                event: Some("y".into()),
                data: "v".into()
            }]
        );

        // With a following byte the ambiguity resolves without a flush.
        let mut resolved = FrameDecoder::new();
        assert_eq!(
            resolved.push(b"event: z\rdata: w\r\rnext"),
            vec![SseFrame {
                event: Some("z".into()),
                data: "w".into()
            }]
        );
    }

    /// A CRLF split across two reads must not read as CR + blank line.
    #[test]
    fn a_crlf_split_across_reads_is_one_terminator() {
        let mut decoder = FrameDecoder::new();
        assert!(decoder.push(b"event: a\r").is_empty());
        assert!(decoder.push(b"\ndata: 1\r").is_empty());
        let frames = decoder.push(b"\n\r\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                event: Some("a".into()),
                data: "1".into()
            }]
        );
    }

    fn tool_stream(seed_identity: bool) -> String {
        let added = if seed_identity {
            json!({"output_index": 0, "item": {
                "type": "function_call", "call_id": "call_a", "name": "weather",
                "arguments": ""}})
        } else {
            // Identity withheld until output_item.done -- the case that must
            // not produce a half-formed tool block.
            json!({"output_index": 0, "item": {"type": "function_call"}})
        };
        [
            upstream("response.created", json!({})),
            upstream("response.output_item.added", added),
            upstream(
                "response.function_call_arguments.delta",
                json!({"output_index": 0, "delta": "{\"city\":"}),
            ),
            upstream(
                "response.function_call_arguments.delta",
                json!({"output_index": 0, "delta": "\"Paris\"}"}),
            ),
            upstream(
                "response.output_item.done",
                json!({"output_index": 0, "item": {
                    "type": "function_call", "call_id": "call_a", "name": "weather",
                    "arguments": "{\"city\":\"Paris\"}"}}),
            ),
            upstream("response.completed", json!({"response": {"usage": {}}})),
        ]
        .concat()
    }

    #[test]
    fn tool_call_with_known_identity_streams_argument_deltas() {
        let frames = run(&tool_stream(true), 4096);
        assert_eq!(
            events(&frames),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let bodies = bodies(&frames);
        assert_eq!(bodies[1]["content_block"]["type"], "tool_use");
        assert_eq!(bodies[1]["content_block"]["id"], "call_a");
        assert_eq!(bodies[1]["content_block"]["name"], "weather");
        let joined = format!(
            "{}{}",
            bodies[2]["delta"]["partial_json"].as_str().unwrap(),
            bodies[3]["delta"]["partial_json"].as_str().unwrap()
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&joined).unwrap(),
            json!({"city": "Paris"})
        );
        // stop_reason must reflect that the turn ended in a tool call.
        assert_eq!(bodies[5]["delta"]["stop_reason"], "tool_use");
    }

    /// The invariant: no `content_block_start` before id and name are known,
    /// and no argument fragment addressed to a block that does not exist yet.
    #[test]
    fn tool_call_with_late_identity_buffers_until_the_block_is_legal() {
        let frames = run(&tool_stream(false), 4096);
        assert_eq!(
            events(&frames),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let bodies = bodies(&frames);
        assert_eq!(bodies[1]["content_block"]["id"], "call_a");
        assert_eq!(bodies[1]["content_block"]["name"], "weather");
        // Both buffered fragments were flushed once, in order, and form
        // exactly the arguments -- not duplicated by output_item.done.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                bodies[2]["delta"]["partial_json"].as_str().unwrap()
            )
            .unwrap(),
            json!({"city": "Paris"})
        );
    }

    #[test]
    fn parallel_tool_calls_get_distinct_monotonic_indices() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.output_item.added",
                json!({"output_index": 0, "item": {
                    "type": "function_call", "call_id": "a", "name": "one"}}),
            ),
            upstream(
                "response.output_item.added",
                json!({"output_index": 1, "item": {
                    "type": "function_call", "call_id": "b", "name": "two"}}),
            ),
            upstream(
                "response.function_call_arguments.delta",
                json!({"output_index": 1, "delta": "{}"}),
            ),
            upstream(
                "response.output_item.done",
                json!({"output_index": 1, "item": {
                    "type": "function_call", "call_id": "b", "name": "two"}}),
            ),
            upstream(
                "response.output_item.done",
                json!({"output_index": 0, "item": {
                    "type": "function_call", "call_id": "a", "name": "one"}}),
            ),
            upstream("response.completed", json!({})),
        ]
        .concat();

        let frames = run(&stream, 4096);
        let bodies = bodies(&frames);
        assert_eq!(bodies[1]["index"], 0);
        assert_eq!(bodies[1]["content_block"]["id"], "a");
        assert_eq!(bodies[2]["index"], 1);
        assert_eq!(bodies[2]["content_block"]["id"], "b");
        // Deltas and stops address their own block, out of allocation order.
        assert_eq!(bodies[3]["index"], 1);
        assert_eq!(bodies[4]["index"], 1);
        assert_eq!(bodies[5]["index"], 0);
        assert_balanced(&frames);
    }

    #[test]
    fn text_and_tool_blocks_interleave_with_monotonic_indices() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "let me check"}),
            ),
            upstream(
                "response.output_text.done",
                json!({"output_index": 0, "content_index": 0}),
            ),
            upstream(
                "response.output_item.added",
                json!({"output_index": 1, "item": {
                    "type": "function_call", "call_id": "c1", "name": "t"}}),
            ),
            upstream(
                "response.output_item.done",
                json!({"output_index": 1, "item": {
                    "type": "function_call", "call_id": "c1", "name": "t"}}),
            ),
            upstream("response.completed", json!({})),
        ]
        .concat();
        let frames = run(&stream, 4096);
        let bodies = bodies(&frames);
        assert_eq!(bodies[1]["content_block"]["type"], "text");
        assert_eq!(bodies[1]["index"], 0);
        assert_eq!(bodies[4]["content_block"]["type"], "tool_use");
        assert_eq!(bodies[4]["index"], 1);
        assert_balanced(&frames);
    }

    /// Every started block is stopped exactly once, message_stop is last, and
    /// nothing follows it.
    fn assert_balanced(frames: &[String]) {
        let names = events(frames);
        let bodies = bodies(frames);
        assert_eq!(names.first().map(String::as_str), Some("message_start"));
        assert_eq!(names.last().map(String::as_str), Some("message_stop"));
        let mut open = std::collections::HashSet::new();
        for (name, body) in names.iter().zip(bodies.iter()) {
            match name.as_str() {
                "content_block_start" => {
                    let index = body["index"].as_u64().unwrap();
                    assert!(open.insert(index), "block {index} started twice");
                }
                "content_block_delta" => {
                    let index = body["index"].as_u64().unwrap();
                    assert!(open.contains(&index), "delta for unopened block {index}");
                }
                "content_block_stop" => {
                    let index = body["index"].as_u64().unwrap();
                    assert!(open.remove(&index), "stop for unopened block {index}");
                }
                _ => {}
            }
        }
        assert!(open.is_empty(), "blocks left open: {open:?}");
    }

    #[test]
    fn malformed_json_and_done_sentinels_are_skipped_not_fatal() {
        let stream = [
            upstream("response.created", json!({})),
            "event: ping\ndata: not json at all\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "ok"}),
            ),
            upstream("response.completed", json!({})),
        ]
        .concat();
        let frames = run(&stream, 4096);
        assert_eq!(bodies(&frames)[2]["delta"]["text"], "ok");
        assert_balanced(&frames);
    }

    #[test]
    fn upstream_error_before_any_output_is_sanitized() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.failed",
                json!({"response": {"error": {
                    "message": "Incorrect API key sk-secret-123 for org org_42"}}}),
            ),
        ]
        .concat();
        let frames = run(&stream, 4096);
        assert_eq!(events(&frames), vec!["message_start", "error"]);
        let rendered = frames.concat();
        assert!(!rendered.contains("sk-secret-123"));
        assert!(!rendered.contains("org_42"));
        // The secrecy invariant is unchanged; what changed is that a failure
        // with no recognised class still gets our generic text rather than the
        // body.
        assert!(rendered.contains("upstream provider error"));
    }

    /// The reported failure delivered *inside* a 200 stream. The body is still
    /// never echoed -- but it is now read, so the frame can say what happened
    /// instead of collapsing onto one constant.
    #[test]
    fn an_in_band_quota_failure_is_classified_and_named() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.failed",
                json!({"response": {"error": {
                    "code": "usage_limit_reached",
                    "message": "You have exhausted your credits for account acct_42"}}}),
            ),
        ]
        .concat();
        let frames = run(&stream, 4096);
        let rendered = frames.concat();
        assert!(rendered.contains("billing_error"), "{rendered}");
        assert!(rendered.contains("quota exhausted"), "{rendered}");
        // Derived, not echoed.
        assert!(!rendered.contains("acct_42"), "{rendered}");
    }

    /// An error after partial output must still close what it opened, or the
    /// client is left holding an unterminated block.
    #[test]
    fn error_after_partial_output_closes_open_blocks_first() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "partial"}),
            ),
            upstream("error", json!({"message": "boom sk-leak"})),
        ]
        .concat();
        let frames = run(&stream, 4096);
        assert_eq!(
            events(&frames),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "error",
            ]
        );
        assert!(!frames.concat().contains("sk-leak"));
    }

    #[test]
    fn a_truncated_stream_is_still_terminated() {
        // Upstream disconnects after one delta: no completed, no blank line.
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "half"}),
            ),
        ]
        .concat();
        let frames = run(&stream, 4096);
        assert_balanced(&frames);
        assert_eq!(
            events(&frames).last().map(String::as_str),
            Some("message_stop")
        );
    }

    #[test]
    fn incomplete_response_maps_to_max_tokens() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "x"}),
            ),
            upstream(
                "response.incomplete",
                json!({"response": {"usage": {"input_tokens": 3, "output_tokens": 1}}}),
            ),
        ]
        .concat();
        let frames = run(&stream, 4096);
        let bodies = bodies(&frames);
        let delta = bodies
            .iter()
            .find(|body| body["type"] == "message_delta")
            .unwrap();
        assert_eq!(delta["delta"]["stop_reason"], "max_tokens");
        assert_eq!(delta["usage"]["output_tokens"], 1);
    }

    #[test]
    fn events_after_completion_are_ignored() {
        let mut translator = StreamTranslator::new("m", "id");
        let created = SseFrame {
            event: Some("response.created".into()),
            data: json!({"type": "response.created"}).to_string(),
        };
        assert!(!translator.push(&created).is_empty());
        let completed = SseFrame {
            event: Some("response.completed".into()),
            data: json!({"type": "response.completed"}).to_string(),
        };
        assert!(!translator.push(&completed).is_empty());
        assert!(translator.is_finished());
        // Late frames, a second finish, and a late failure are all no-ops.
        assert!(translator.push(&created).is_empty());
        assert!(translator.finish().is_empty());
        assert!(translator.fail_opaque().is_empty());
    }

    /// #750 A5: reasoning round-trips. The Anthropic `signature` is the
    /// reasoning item's `encrypted_content`, verbatim -- that is what the
    /// client echoes back so the next turn keeps its reasoning under
    /// `store: false`.
    #[test]
    fn reasoning_becomes_a_signed_thinking_block() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.reasoning_summary_text.delta",
                json!({"output_index": 0, "delta": "first thought"}),
            ),
            upstream(
                "response.reasoning_summary_part.added",
                json!({"output_index": 0}),
            ),
            upstream(
                "response.reasoning_summary_text.delta",
                json!({"output_index": 0, "delta": "second thought"}),
            ),
            upstream(
                "response.output_item.done",
                json!({"output_index": 0, "item": {
                    "type": "reasoning", "encrypted_content": "gAAAA-signature"}}),
            ),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 1, "content_index": 0, "delta": "answer"}),
            ),
            upstream("response.completed", json!({})),
        ]
        .concat();
        let frames = run(&stream, 4096);
        let names = events(&frames);
        let bodies = bodies(&frames);

        assert_eq!(bodies[1]["content_block"]["type"], "thinking");
        // Several summary parts stay in ONE block, joined by a blank line.
        let joined: String = bodies
            .iter()
            .filter(|body| body["delta"]["type"] == "thinking_delta")
            .map(|body| body["delta"]["thinking"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            joined,
            "first thought

second thought"
        );

        // Signature emitted exactly once, immediately before the stop.
        let signature_positions: Vec<usize> = bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| body["delta"]["type"] == "signature_delta")
            .map(|(index, _)| index)
            .collect();
        assert_eq!(signature_positions.len(), 1);
        let at = signature_positions[0];
        assert_eq!(bodies[at]["delta"]["signature"], "gAAAA-signature");
        assert_eq!(names[at + 1], "content_block_stop");
        assert_balanced(&frames);
    }

    /// A reasoning item can carry a signature with no summary text at all.
    /// That signature is still the round-trip carrier, so an empty thinking
    /// block is opened to deliver it.
    #[test]
    fn a_signature_only_reasoning_item_still_reaches_the_client() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.output_item.done",
                json!({"output_index": 0, "item": {
                    "type": "reasoning", "encrypted_content": "gAAAA-only"}}),
            ),
            upstream("response.completed", json!({})),
        ]
        .concat();
        let frames = run(&stream, 4096);
        let rendered = frames.concat();
        assert!(rendered.contains("\"type\":\"thinking\""));
        assert!(rendered.contains("gAAAA-only"));
        assert_balanced(&frames);
    }

    /// Structural invariants that must hold for *any* input, including
    /// corrupt or truncated streams. Looser than `assert_balanced`: a stream
    /// may legitimately end in `error` instead of `message_stop`.
    fn assert_structurally_sound(frames: &[String], context: &str) {
        if frames.is_empty() {
            return;
        }
        let names = events(frames);
        assert_eq!(
            names.first().map(String::as_str),
            Some("message_start"),
            "{context}: stream did not open with message_start"
        );
        let terminal = names.last().map(String::as_str);
        assert!(
            matches!(terminal, Some("message_stop") | Some("error")),
            "{context}: unterminated stream, ended with {terminal:?}"
        );
        assert_eq!(
            names.iter().filter(|name| *name == "message_start").count(),
            1,
            "{context}: more than one message_start"
        );
        let mut open = std::collections::HashSet::new();
        for (name, body) in names.iter().zip(bodies(frames).iter()) {
            match name.as_str() {
                "content_block_start" => {
                    let index = body["index"].as_u64().unwrap();
                    assert!(open.insert(index), "{context}: block {index} started twice");
                    // A tool block must never open without both identifiers.
                    if body["content_block"]["type"] == "tool_use" {
                        assert!(
                            body["content_block"]["id"]
                                .as_str()
                                .is_some_and(|v| !v.is_empty()),
                            "{context}: tool block without an id"
                        );
                        assert!(
                            body["content_block"]["name"]
                                .as_str()
                                .is_some_and(|v| !v.is_empty()),
                            "{context}: tool block without a name"
                        );
                    }
                }
                "content_block_delta" => {
                    let index = body["index"].as_u64().unwrap();
                    assert!(
                        open.contains(&index),
                        "{context}: delta addressed unopened block {index}"
                    );
                }
                "content_block_stop" => {
                    let index = body["index"].as_u64().unwrap();
                    assert!(
                        open.remove(&index),
                        "{context}: stop for unopened block {index}"
                    );
                }
                _ => {}
            }
        }
        assert!(open.is_empty(), "{context}: blocks left open: {open:?}");
    }

    /// Exhaustive fragmentation sweep over a stream containing text, a tool
    /// call, CRLF, and heartbeats. Every split point must be equivalent.
    #[test]
    fn every_split_point_of_a_mixed_stream_is_equivalent() {
        let stream = format!(
            "{}: hb\r\n\r\n{}",
            text_stream(),
            tool_stream(true).replace('\n', "\r\n")
        );
        let reference = run(&stream, stream.len());
        assert_structurally_sound(&reference, "reference");
        for chunk in 1..=stream.len().min(80) {
            assert_eq!(
                run(&stream, chunk),
                reference,
                "fragmentation at {chunk} bytes changed the output"
            );
        }
    }

    /// Deterministic fuzz: corrupt a valid stream in many ways and assert the
    /// translator never panics and never emits a structurally invalid
    /// sequence. Seeded so a failure is reproducible from the printed context.
    #[test]
    fn corrupted_streams_never_panic_or_emit_invalid_structure() {
        // The tool call comes first and carries its identity up front, so
        // truncation cases still reach the tool path. With the tool call at
        // the tail, the fuzz opened a tool block in only 12/400 runs and the
        // most delicate code went essentially unexercised.
        let base = [
            upstream("response.created", json!({})),
            upstream(
                "response.output_item.added",
                json!({"output_index": 0, "item": {
                    "type": "function_call", "call_id": "call_z", "name": "search"}}),
            ),
            upstream(
                "response.function_call_arguments.delta",
                json!({"output_index": 0, "delta": "{\"q\":"}),
            ),
            upstream(
                "response.function_call_arguments.delta",
                json!({"output_index": 0, "delta": "\"rust\"}"}),
            ),
            upstream(
                "response.output_item.done",
                json!({"output_index": 0, "item": {
                    "type": "function_call", "call_id": "call_z", "name": "search"}}),
            ),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 1, "content_index": 0, "delta": "done"}),
            ),
            upstream(
                "response.output_text.done",
                json!({"output_index": 1, "content_index": 0}),
            ),
            upstream(
                "response.completed",
                json!({"response": {"usage": {"input_tokens": 5, "output_tokens": 9}}}),
            ),
        ]
        .concat();
        let bytes = base.as_bytes();
        let mut state: u64 = 0x5eed_1234_dead_beef;
        let mut next = move || {
            // xorshift64*: no dependency, and reproducible across platforms.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };

        let mut produced_output = 0_usize;
        let mut produced_tool_block = 0_usize;
        for case in 0..400 {
            let mut corrupted = bytes.to_vec();
            match case % 4 {
                // Truncate at an arbitrary point (mid-stream disconnect).
                0 => corrupted.truncate((next() as usize) % bytes.len().max(1)),
                // Flip one byte (malformed JSON, broken framing).
                1 => {
                    let at = (next() as usize) % bytes.len();
                    corrupted[at] ^= 1 << (next() % 8);
                }
                // Delete a run (lost segment).
                2 => {
                    let at = (next() as usize) % bytes.len();
                    let len = ((next() as usize) % 32).min(bytes.len() - at);
                    corrupted.drain(at..at + len);
                }
                // Inject noise (heartbeats, partial frames, stray delimiters).
                _ => {
                    let at = (next() as usize) % bytes.len();
                    let noise: &[u8] = match next() % 4 {
                        0 => b"\n\n",
                        1 => b": heartbeat\n\n",
                        2 => b"data: {\"type\":\"unknown.event\"}\n\n",
                        _ => b"\r",
                    };
                    corrupted.splice(at..at, noise.iter().copied());
                }
            }

            let chunk = 1 + (next() as usize) % 17;
            let mut decoder = FrameDecoder::new();
            let mut translator = StreamTranslator::new("gpt-5.6-sol", "msg_fuzz");
            let mut out = Vec::new();
            for piece in corrupted.chunks(chunk) {
                for frame in decoder.push(piece) {
                    out.extend(translator.push(&frame));
                }
            }
            for frame in decoder.finish() {
                out.extend(translator.push(&frame));
            }
            out.extend(translator.finish());
            assert_structurally_sound(&out, &format!("case {case} chunk {chunk}"));
            if !out.is_empty() {
                produced_output += 1;
            }
            if out.iter().any(|frame| frame.contains("\"tool_use\"")) {
                produced_tool_block += 1;
            }
        }
        // Guard against a vacuous fuzz: `assert_structurally_sound` passes
        // trivially on an empty stream, so assert the corpus actually reached
        // the interesting paths.
        assert!(
            produced_output > 300,
            "fuzz produced output in only {produced_output}/400 cases"
        );
        assert!(
            produced_tool_block > 300,
            "fuzz opened a tool block in only {produced_tool_block}/400 cases"
        );
    }

    /// Emitted frames must be parseable as SSE by a strict reader.
    #[test]
    fn emitted_frames_are_well_formed_sse() {
        for frame in run(&tool_stream(true), 7) {
            assert!(frame.starts_with("event: "), "{frame:?}");
            assert!(frame.ends_with("\n\n"), "{frame:?}");
            let data = frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .expect("data line");
            assert!(!data.contains('\n'));
            serde_json::from_str::<serde_json::Value>(data).expect("JSON body");
        }
    }
}
