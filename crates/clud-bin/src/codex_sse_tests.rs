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
fn invalid_request_categories_are_safe_and_specific() {
    let cases = [
        (
            json!({"response": {"error": {"type": "invalid_request", "code": "context_length_exceeded", "message": "SECRET"}, "request_id": "req_context"}}),
            "context_length",
            Some("context_length_exceeded"),
            "context is too long",
        ),
        (
            json!({"response": {"error": {"code": "cyber_policy", "message": "SECRET"}}}),
            "policy",
            Some("cyber_policy"),
            "provider policy",
        ),
        (
            json!({"response": {"error": {"type": "invalid_request", "message": "SECRET"}}}),
            "malformed_request",
            None,
            "unsupported or malformed",
        ),
        (
            json!({"response": {"error": {"message": "SECRET"}}}),
            "unknown_invalid_request",
            None,
            "unclassified",
        ),
    ];
    for (upstream, category, code, message) in cases {
        let failure = InBandFailure::from_upstream(&upstream);
        assert_eq!(failure.category, category);
        assert_eq!(failure.code, code);
        assert!(failure.client_message().contains(message));
        assert!(!failure.client_message().contains("SECRET"));
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
