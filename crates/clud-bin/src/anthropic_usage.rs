//! Terminal usage adapters for forwarded Anthropic-compatible SSE streams.
//!
//! The bridge forwards original bytes unchanged. This module observes a
//! bounded framing copy only, retains numeric terminal counters, and never
//! stores message content, credentials, or session identifiers.

use crate::cache_health::TokenUsage;
use crate::codex_sse::FrameDecoder;

const MAX_DIRECT_JSON_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    DeepSeek,
    Kimi,
    OpenRouter,
}

pub struct StreamingUsage {
    provider: Provider,
    frames: FrameDecoder,
    response_form: ResponseForm,
    direct_json: Vec<u8>,
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseForm {
    Unknown,
    DirectJson,
    Sse,
    TooLarge,
}

impl StreamingUsage {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            frames: FrameDecoder::new(),
            response_form: ResponseForm::Unknown,
            direct_json: Vec::new(),
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.observe_response_form(bytes);
        for frame in self.frames.push(bytes) {
            self.observe(&frame.data);
        }
    }

    pub fn finish(mut self) -> Option<TokenUsage> {
        if self.response_form == ResponseForm::DirectJson {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&self.direct_json) else {
                return None;
            };
            self.observe_value(&value);
        }
        for frame in self.frames.finish() {
            self.observe(&frame.data);
        }
        let usage = TokenUsage {
            input_tokens: self.input_tokens?,
            cached_input_tokens: self.cached_input_tokens.unwrap_or(0),
            output_tokens: self.output_tokens?,
        };
        usage.is_valid().then_some(usage)
    }

    fn observe(&mut self, data: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        self.observe_value(&value);
    }

    fn observe_response_form(&mut self, bytes: &[u8]) {
        if self.response_form == ResponseForm::Unknown {
            let Some(first) = bytes
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
            else {
                return;
            };
            self.response_form = if first == b'{' {
                ResponseForm::DirectJson
            } else {
                ResponseForm::Sse
            };
        }
        if self.response_form != ResponseForm::DirectJson {
            return;
        }
        let remaining = MAX_DIRECT_JSON_BYTES.saturating_sub(self.direct_json.len());
        if bytes.len() > remaining {
            self.direct_json.clear();
            self.response_form = ResponseForm::TooLarge;
            return;
        }
        self.direct_json.extend_from_slice(bytes);
    }

    fn observe_value(&mut self, value: &serde_json::Value) {
        let usage = match value.get("type").and_then(serde_json::Value::as_str) {
            Some("message_start") => value.pointer("/message/usage"),
            Some("message_delta") | Some("message_stop") => value.get("usage"),
            _ => value.get("usage"),
        };
        let Some(usage) = usage else {
            return;
        };
        match self.provider {
            Provider::Claude => self.observe_claude(usage),
            Provider::DeepSeek => self.observe_deepseek(usage),
            // Moonshot's Anthropic endpoint reports Claude-shaped usage; the
            // OpenRouter adapter reads that shape and keeps the permissive
            // OpenAI-field fallback for anything else.
            Provider::Kimi | Provider::OpenRouter => self.observe_openrouter(usage),
        }
    }

    fn observe_claude(&mut self, usage: &serde_json::Value) {
        let ordinary = number(usage, "input_tokens");
        let created = number(usage, "cache_creation_input_tokens").unwrap_or(0);
        let read = number(usage, "cache_read_input_tokens").unwrap_or(0);
        if ordinary.is_some() || created != 0 || read != 0 {
            self.input_tokens = Some(
                ordinary
                    .unwrap_or(0)
                    .saturating_add(created)
                    .saturating_add(read),
            );
            self.cached_input_tokens = Some(read);
        }
        if let Some(output) = number(usage, "output_tokens") {
            self.output_tokens = Some(output);
        }
    }

    fn observe_deepseek(&mut self, usage: &serde_json::Value) {
        let hit = number(usage, "prompt_cache_hit_tokens");
        let miss = number(usage, "prompt_cache_miss_tokens");
        if let (Some(hit), Some(miss)) = (hit, miss) {
            self.input_tokens = Some(hit.saturating_add(miss));
            self.cached_input_tokens = Some(hit);
        } else {
            self.observe_total_and_cached(usage);
        }
        if let Some(output) =
            number(usage, "output_tokens").or_else(|| number(usage, "completion_tokens"))
        {
            self.output_tokens = Some(output);
        }
    }

    fn observe_openrouter(&mut self, usage: &serde_json::Value) {
        // The bridge calls OpenRouter's Anthropic Messages endpoint, whose
        // documented usage shape is Claude-compatible. Do not mistake a
        // cache read for uncached input simply because OpenRouter also offers
        // OpenAI-shaped endpoints elsewhere.
        let anthropic_shape = number(usage, "input_tokens").is_some()
            || number(usage, "cache_creation_input_tokens").is_some()
            || number(usage, "cache_read_input_tokens").is_some();
        // `message_delta` carries only `output_tokens`, so always let the
        // Anthropic adapter see it even when this frame has no input fields.
        self.observe_claude(usage);
        if !anthropic_shape {
            // Keep the permissive fallback for an intermediary that returns
            // OpenAI-compatible terminal fields on an Anthropic route.
            self.observe_total_and_cached(usage);
            if let Some(output) = number(usage, "completion_tokens") {
                self.output_tokens = Some(output);
            }
        }
    }

    fn observe_total_and_cached(&mut self, usage: &serde_json::Value) {
        if let Some(input) =
            number(usage, "input_tokens").or_else(|| number(usage, "prompt_tokens"))
        {
            self.input_tokens = Some(input);
        }
        if let Some(cached) = usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| {
                usage
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(serde_json::Value::as_u64)
            })
        {
            self.cached_input_tokens = Some(cached);
        }
    }
}

fn number(value: &serde_json::Value, name: &str) -> Option<u64> {
    value.get(name).and_then(serde_json::Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(provider: Provider, events: &str) -> Option<TokenUsage> {
        let mut tracker = StreamingUsage::new(provider);
        for chunk in events.as_bytes().chunks(7) {
            tracker.feed(chunk);
        }
        tracker.finish()
    }

    #[test]
    fn claude_counts_cache_read_and_creation_as_distinct_input_categories() {
        let events = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_creation_input_tokens\":20,\"cache_read_input_tokens\":30}}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n\n";
        assert_eq!(
            usage(Provider::Claude, events),
            Some(TokenUsage {
                input_tokens: 60,
                cached_input_tokens: 30,
                output_tokens: 4
            })
        );
    }

    #[test]
    fn deepseek_uses_its_disjoint_cache_hit_and_miss_counters() {
        let events = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"prompt_cache_hit_tokens\":80,\"prompt_cache_miss_tokens\":20}}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"completion_tokens\":3}}\n\n";
        assert_eq!(
            usage(Provider::DeepSeek, events),
            Some(TokenUsage {
                input_tokens: 100,
                cached_input_tokens: 80,
                output_tokens: 3
            })
        );
    }

    #[test]
    fn openrouter_uses_anthropic_messages_cache_read_counters() {
        let events = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_creation_input_tokens\":5,\"cache_read_input_tokens\":35}}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2}}\n\n";
        assert_eq!(
            usage(Provider::OpenRouter, events),
            Some(TokenUsage {
                input_tokens: 50,
                cached_input_tokens: 35,
                output_tokens: 2
            })
        );
    }

    #[test]
    fn openrouter_accepts_openai_compatible_usage_as_a_last_resort() {
        let events = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"prompt_tokens\":50,\"prompt_tokens_details\":{\"cached_tokens\":40}}}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"completion_tokens\":2}}\n\n";
        assert_eq!(
            usage(Provider::OpenRouter, events),
            Some(TokenUsage {
                input_tokens: 50,
                cached_input_tokens: 40,
                output_tokens: 2
            })
        );
    }

    #[test]
    fn malformed_or_impossible_usage_is_unavailable() {
        let events = "data: {not-json}\n\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"prompt_tokens\":1,\"prompt_tokens_details\":{\"cached_tokens\":2}}}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"completion_tokens\":0}}\n\n";
        assert_eq!(usage(Provider::OpenRouter, events), None);
    }

    #[test]
    fn direct_json_usage_is_observed_without_retaining_an_unbounded_body() {
        let mut tracker = StreamingUsage::new(Provider::Claude);
        tracker.feed(
            br#"{"usage":{"input_tokens":10,"cache_read_input_tokens":6,"output_tokens":2}}"#,
        );
        assert_eq!(
            tracker.finish(),
            Some(TokenUsage {
                input_tokens: 16,
                cached_input_tokens: 6,
                output_tokens: 2,
            })
        );

        let mut oversized = StreamingUsage::new(Provider::Claude);
        let mut body = vec![b'x'; MAX_DIRECT_JSON_BYTES + 1];
        body[0] = b'{';
        oversized.feed(&body);
        assert_eq!(oversized.finish(), None);
    }
}
