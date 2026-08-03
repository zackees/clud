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

#[derive(Debug)]
struct CallState {
    index: Option<u32>,
    call_id: Option<String>,
    name: Option<String>,
    /// Argument text that arrived before the block could legally be opened.
    buffered_arguments: String,
    closed: bool,
}

/// Responses events -> Anthropic events.
///
/// Emits complete SSE frames as strings so the caller can write each one and
/// flush, which is what makes the turn render progressively.
#[derive(Debug)]
pub struct StreamTranslator {
    model: String,
    message_id: String,
    started: bool,
    finished: bool,
    next_index: u32,
    text_indices: HashMap<(i64, i64), u32>,
    open_blocks: Vec<u32>,
    calls: HashMap<i64, CallState>,
    saw_tool_call: bool,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<&'static str>,
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
            calls: HashMap::new(),
            saw_tool_call: false,
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
        }
    }

    /// Translate one upstream frame into zero or more Anthropic frames.
    pub fn push(&mut self, frame: &SseFrame) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        // The Responses stream carries its type in the JSON body; the SSE
        // `event:` field mirrors it. Prefer the body, fall back to the field.
        let value: serde_json::Value = match serde_json::from_str(&frame.data) {
            Ok(value) => value,
            Err(_) => {
                // `[DONE]` sentinels and other non-JSON payloads are not
                // errors; they simply carry nothing to translate.
                return Vec::new();
            }
        };
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .or(frame.event.as_deref())
            .unwrap_or("")
            .to_string();

        let mut out = Vec::new();
        match kind.as_str() {
            "response.created" | "response.in_progress" => {
                self.ensure_started(&mut out);
            }
            "response.output_text.delta" => {
                self.ensure_started(&mut out);
                let key = (
                    json_i64(&value, "output_index").unwrap_or(0),
                    json_i64(&value, "content_index").unwrap_or(0),
                );
                let index = self.ensure_text_block(key, &mut out);
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    out.push(anthropic_frame(
                        "content_block_delta",
                        serde_json::json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {"type": "text_delta", "text": delta},
                        }),
                    ));
                }
            }
            "response.content_part.done" | "response.output_text.done" => {
                let key = (
                    json_i64(&value, "output_index").unwrap_or(0),
                    json_i64(&value, "content_index").unwrap_or(0),
                );
                if let Some(index) = self.text_indices.get(&key).copied() {
                    self.close_block(index, &mut out);
                }
            }
            "response.output_item.added" => {
                self.ensure_started(&mut out);
                let output_index = json_i64(&value, "output_index").unwrap_or(0);
                if let Some(item) = value.get("item") {
                    if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                    {
                        self.saw_tool_call = true;
                        let entry = self.calls.entry(output_index).or_insert(CallState {
                            index: None,
                            call_id: None,
                            name: None,
                            buffered_arguments: String::new(),
                            closed: false,
                        });
                        merge_call_identity(entry, item);
                        let seed = item
                            .get("arguments")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        entry.buffered_arguments.push_str(seed);
                        self.try_open_call(output_index, &mut out);
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                self.ensure_started(&mut out);
                let output_index = json_i64(&value, "output_index").unwrap_or(0);
                let delta = value
                    .get("delta")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let entry = self.calls.entry(output_index).or_insert(CallState {
                    index: None,
                    call_id: None,
                    name: None,
                    buffered_arguments: String::new(),
                    closed: false,
                });
                self.saw_tool_call = true;
                match entry.index {
                    // Block is open: forward the fragment as-is. Anthropic
                    // explicitly allows partial (invalid) JSON here.
                    Some(index) => out.push(anthropic_frame(
                        "content_block_delta",
                        serde_json::json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {"type": "input_json_delta", "partial_json": delta},
                        }),
                    )),
                    // Identity still unknown: hold the fragment rather than
                    // address a block that does not exist.
                    None => entry.buffered_arguments.push_str(delta),
                }
            }
            "response.function_call_arguments.done" => {
                let output_index = json_i64(&value, "output_index").unwrap_or(0);
                if let Some(arguments) = value.get("arguments").and_then(serde_json::Value::as_str)
                {
                    if let Some(entry) = self.calls.get_mut(&output_index) {
                        // Only adopt the terminal value when nothing was
                        // streamed; otherwise this would duplicate the body.
                        if entry.index.is_none() && entry.buffered_arguments.is_empty() {
                            entry.buffered_arguments.push_str(arguments);
                        }
                    }
                }
            }
            "response.output_item.done" => {
                let output_index = json_i64(&value, "output_index").unwrap_or(0);
                if let Some(item) = value.get("item") {
                    if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                    {
                        if let Some(entry) = self.calls.get_mut(&output_index) {
                            merge_call_identity(entry, item);
                            if entry.index.is_none() && entry.buffered_arguments.is_empty() {
                                if let Some(arguments) =
                                    item.get("arguments").and_then(serde_json::Value::as_str)
                                {
                                    entry.buffered_arguments.push_str(arguments);
                                }
                            }
                        }
                        self.try_open_call(output_index, &mut out);
                        if let Some(index) =
                            self.calls.get(&output_index).and_then(|entry| entry.index)
                        {
                            self.close_block(index, &mut out);
                            if let Some(entry) = self.calls.get_mut(&output_index) {
                                entry.closed = true;
                            }
                        }
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                self.absorb_usage(&value);
                if self.stop_reason.is_none() {
                    self.stop_reason = Some(if kind == "response.incomplete" {
                        "max_tokens"
                    } else if self.saw_tool_call {
                        "tool_use"
                    } else {
                        "end_turn"
                    });
                }
                out.extend(self.finish());
            }
            "response.failed" | "error" => {
                out.extend(self.fail());
            }
            // Reasoning summaries are intentionally not forwarded. An
            // Anthropic `thinking` block carries a signature that the
            // Responses API does not provide, and clients reject an unsigned
            // one -- emitting it would break the very turn it decorates.
            // Revisit if a signed equivalent appears.
            _ => {}
        }
        out
    }

    /// Close the turn: shut any open blocks, then send message_delta/stop.
    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        for index in std::mem::take(&mut self.open_blocks) {
            out.push(anthropic_frame(
                "content_block_stop",
                serde_json::json!({"type": "content_block_stop", "index": index}),
            ));
        }
        let stop_reason = self.stop_reason.unwrap_or(if self.saw_tool_call {
            "tool_use"
        } else {
            "end_turn"
        });
        out.push(anthropic_frame(
            "message_delta",
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                // input_tokens is only known once upstream reports usage, which
                // is long after message_start has been flushed. Carrying it
                // here is the only chance the client gets to learn it.
                "usage": {
                    "input_tokens": self.input_tokens,
                    "output_tokens": self.output_tokens,
                },
            }),
        ));
        out.push(anthropic_frame(
            "message_stop",
            serde_json::json!({"type": "message_stop"}),
        ));
        self.finished = true;
        out
    }

    /// Terminate the stream with a sanitized error.
    ///
    /// The upstream body is deliberately not echoed: it can carry account
    /// identifiers or key fragments, and this frame is written straight to the
    /// harness.
    pub fn fail(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        for index in std::mem::take(&mut self.open_blocks) {
            out.push(anthropic_frame(
                "content_block_stop",
                serde_json::json!({"type": "content_block_stop", "index": index}),
            ));
        }
        out.push(anthropic_frame(
            "error",
            serde_json::json!({
                "type": "error",
                "error": {"type": "api_error", "message": "upstream provider error"},
            }),
        ));
        self.finished = true;
        out
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
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": 0,
                    },
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

    /// Open a tool block once -- and only once -- its id and name are known,
    /// flushing whatever arguments were buffered while waiting.
    fn try_open_call(&mut self, output_index: i64, out: &mut Vec<String>) {
        let Some(entry) = self.calls.get(&output_index) else {
            return;
        };
        if entry.index.is_some() || entry.closed {
            return;
        }
        let (Some(call_id), Some(name)) = (entry.call_id.clone(), entry.name.clone()) else {
            return;
        };
        let buffered = entry.buffered_arguments.clone();
        let index = self.allocate_index();
        if let Some(entry) = self.calls.get_mut(&output_index) {
            entry.index = Some(index);
            entry.buffered_arguments.clear();
        }
        self.open_blocks.push(index);
        out.push(anthropic_frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "tool_use", "id": call_id, "name": name, "input": {}},
            }),
        ));
        if !buffered.is_empty() {
            out.push(anthropic_frame(
                "content_block_delta",
                serde_json::json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "input_json_delta", "partial_json": buffered},
                }),
            ));
        }
    }

    fn close_block(&mut self, index: u32, out: &mut Vec<String>) {
        if let Some(position) = self.open_blocks.iter().position(|open| *open == index) {
            self.open_blocks.remove(position);
            out.push(anthropic_frame(
                "content_block_stop",
                serde_json::json!({"type": "content_block_stop", "index": index}),
            ));
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
    }
}

fn merge_call_identity(entry: &mut CallState, item: &serde_json::Value) {
    // `call_id` is the identifier the follow-up tool_result must quote; `id`
    // is the item's own handle. Prefer the former, accept the latter.
    if entry.call_id.is_none() {
        if let Some(call_id) = item
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| item.get("id").and_then(serde_json::Value::as_str))
        {
            if !call_id.is_empty() {
                entry.call_id = Some(call_id.to_string());
            }
        }
    }
    if entry.name.is_none() {
        if let Some(name) = item.get("name").and_then(serde_json::Value::as_str) {
            if !name.is_empty() {
                entry.name = Some(name.to_string());
            }
        }
    }
}

fn json_i64(value: &serde_json::Value, field: &str) -> Option<i64> {
    value.get(field).and_then(serde_json::Value::as_i64)
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
        assert!(rendered.contains("upstream provider error"));
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
        assert!(translator.fail().is_empty());
    }

    #[test]
    fn reasoning_summaries_are_not_forwarded() {
        let stream = [
            upstream("response.created", json!({})),
            upstream(
                "response.reasoning_summary_text.delta",
                json!({"output_index": 0, "delta": "thinking about it"}),
            ),
            upstream(
                "response.output_text.delta",
                json!({"output_index": 0, "content_index": 0, "delta": "answer"}),
            ),
            upstream("response.completed", json!({})),
        ]
        .concat();
        let frames = run(&stream, 4096);
        let rendered = frames.concat();
        assert!(!rendered.contains("thinking about it"));
        assert!(rendered.contains("answer"));
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
