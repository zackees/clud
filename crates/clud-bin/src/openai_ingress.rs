//! OpenAI Chat Completions → Anthropic Messages request translation.
//!
//! This is the ingress half of the OpenAI-compatible gateway surface: an
//! OpenAI-shaped client (`POST /v1/chat/completions`) is rewritten into the
//! Messages request the rest of clud already speaks, so none of the routing,
//! failover, catalog or credential machinery has to learn a second protocol.
//!
//! Ported from CLIProxyAPI
//! `internal/translator/claude/openai/chat-completions/claude_openai_request.go`
//! (`ConvertOpenAIRequestToClaude`). Deliberate differences, all additive:
//!
//! * `metadata.user_id` derivation is not ported — clud forwards what the
//!   caller sent and does not fingerprint the request.
//! * `response_format` is not turned into a synthesized system instruction:
//!   silently injecting prompt text is the kind of hidden payload rewrite
//!   clud avoids.
//! * Unknown/unsupported content parts (audio, uploaded files) are a typed
//!   error naming the part instead of being dropped, so a client learns its
//!   input did not arrive.
//!
//! The response half (`Anthropic JSON/SSE → OpenAI`) and the served route are
//! separate slices; nothing here changes the launch-scoped gateway contract.

use serde_json::{json, Map, Value};

/// Upstream's default when the client omits both `max_tokens` and
/// `max_completion_tokens`. Messages requests must always carry the field.
pub const DEFAULT_MAX_TOKENS: u64 = 32_000;

/// Longest tool name Anthropic accepts.
const MAX_TOOL_NAME: usize = 64;

/// `clud __openai-chat-translate` — the process seam behind the Python
/// subprocess test. Reads one OpenAI-shaped body from stdin and writes the
/// translated Messages body (compact JSON + newline) to stdout.
///
/// It is hidden and takes no session, backend, credential or daemon action, so
/// exercising the translator never starts a harness.
pub fn run_translate_stdin() -> i32 {
    use std::io::{Read, Write};

    let mut body = Vec::new();
    if std::io::stdin().read_to_end(&mut body).is_err() {
        eprintln!("could not read the request body from stdin");
        return 2;
    }
    match translate_chat_request(&body) {
        Ok(translated) => {
            let mut rendered = serde_json::to_vec(&translated).unwrap_or_default();
            rendered.push(b'\n');
            match std::io::stdout().write_all(&rendered) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("could not write the translated request: {error}");
                    2
                }
            }
        }
        Err(error) => {
            eprintln!("{error}");
            2
        }
    }
}

/// Why an OpenAI-shaped request cannot be served as a Messages request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressError {
    /// The body was not a JSON object.
    NotJson,
    /// No `model` string, so no route can be chosen.
    MissingModel,
    /// No usable `messages` (and no system prompt to stand in for one).
    EmptyMessages,
    /// A content part clud has no Anthropic spelling for. Names the part.
    UnsupportedPart(String),
}

impl std::fmt::Display for IngressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson => formatter.write_str("request body is not a JSON object"),
            Self::MissingModel => formatter.write_str("request is missing a model"),
            Self::EmptyMessages => formatter.write_str("request contains no messages"),
            Self::UnsupportedPart(part) => write!(
                formatter,
                "content part `{part}` has no Anthropic equivalent on this gateway"
            ),
        }
    }
}

impl std::error::Error for IngressError {}

/// Translate one OpenAI Chat Completions request body into an Anthropic
/// Messages request body.
///
/// `model` is passed through verbatim: resolution against clud's catalog,
/// allowlists and effort suffixes stays with the gateway that already owns it.
pub fn translate_chat_request(body: &[u8]) -> Result<Value, IngressError> {
    let request: Value = serde_json::from_slice(body).map_err(|_| IngressError::NotJson)?;
    let request = request.as_object().ok_or(IngressError::NotJson)?;

    let model = request
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or(IngressError::MissingModel)?;

    let mut out = Map::new();
    out.insert("model".into(), Value::String(model.to_string()));
    out.insert(
        "max_tokens".into(),
        json!(max_tokens(request).unwrap_or(DEFAULT_MAX_TOKENS)),
    );
    if let Some(top_p) = request.get("top_p").and_then(Value::as_f64) {
        out.insert("top_p".into(), json!(top_p));
    }
    if let Some(temperature) = request.get("temperature").and_then(Value::as_f64) {
        out.insert("temperature".into(), json!(temperature));
    }
    if let Some(stop) = stop_sequences(request.get("stop")) {
        out.insert("stop_sequences".into(), stop);
    }
    out.insert(
        "stream".into(),
        json!(request.get("stream").and_then(Value::as_bool) == Some(true)),
    );
    if let Some(effort) = reasoning_effort(request.get("reasoning_effort")) {
        out.insert("output_config".into(), json!({ "effort": effort }));
    }

    let messages = match request.get("messages").and_then(Value::as_array) {
        Some(messages) => messages.as_slice(),
        None => return Err(IngressError::EmptyMessages),
    };

    let mut system: Vec<Value> = Vec::new();
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();
    for message in messages {
        let message = message.as_object().ok_or(IngressError::NotJson)?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or(IngressError::NotJson)?;
        match role {
            "system" | "developer" => {
                // Developer messages rank with system messages in OpenAI's
                // instruction hierarchy; dropping them loses operator intent.
                for block in text_blocks(message.get("content"))? {
                    system.push(block);
                }
            }
            "user" | "assistant" => {
                let mut blocks = content_blocks(message.get("content"))?;
                if role == "assistant" {
                    blocks.extend(tool_use_blocks(message.get("tool_calls"))?);
                }
                if !blocks.is_empty() {
                    push_turn(&mut turns, role, blocks);
                }
            }
            "tool" => {
                let block = tool_result_block(message)?;
                // Anthropic requires alternating roles, so consecutive tool
                // results coalesce into a single user turn.
                push_turn(&mut turns, "user", vec![block]);
            }
            other => return Err(IngressError::UnsupportedPart(format!("role:{other}"))),
        }
    }

    // A system-only request still needs one conversational turn: an empty
    // `messages` array is not a Messages request.
    if turns.is_empty() {
        if system.is_empty() {
            return Err(IngressError::EmptyMessages);
        }
        turns.push(("user".into(), vec![json!({ "type": "text", "text": "" })]));
    }

    if !system.is_empty() {
        out.insert("system".into(), Value::Array(system));
    }
    out.insert(
        "messages".into(),
        Value::Array(
            turns
                .into_iter()
                .map(|(role, content)| json!({ "role": role, "content": content }))
                .collect(),
        ),
    );

    let tools = tool_definitions(request.get("tools"))?;
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
        if let Some(choice) = tool_choice(request.get("tool_choice")) {
            out.insert("tool_choice".into(), choice);
        }
        if request.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false) {
            let choice = out
                .entry("tool_choice")
                .or_insert_with(|| json!({ "type": "auto" }));
            if choice.get("type").and_then(Value::as_str) != Some("none") {
                choice["disable_parallel_tool_use"] = json!(true);
            }
        }
    }

    Ok(Value::Object(out))
}

/// Merge a new turn into the accumulated alternation.
///
/// Two consecutive messages with the same role are one Anthropic turn; keeping
/// them separate is rejected by the Messages API, and a tool result always
/// follows its assistant turn.
fn push_turn(turns: &mut Vec<(String, Vec<Value>)>, role: &str, blocks: Vec<Value>) {
    match turns.last_mut() {
        Some((previous, content)) if previous == role => content.extend(blocks),
        _ => turns.push((role.to_string(), blocks)),
    }
}

fn max_tokens(request: &Map<String, Value>) -> Option<u64> {
    // OpenAI deprecated `max_tokens` in favour of `max_completion_tokens`, so
    // both spellings are requests for the same Anthropic field.
    request
        .get("max_completion_tokens")
        .or_else(|| request.get("max_tokens"))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn stop_sequences(stop: Option<&Value>) -> Option<Value> {
    match stop? {
        Value::String(single) => Some(json!([single])),
        Value::Array(items) => {
            let sequences: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            (!sequences.is_empty()).then(|| json!(sequences))
        }
        _ => None,
    }
}

/// OpenAI `reasoning_effort` is clud's session-wide effort spelling. `none`
/// disables thinking by omitting the block, matching the catalog's `none`.
fn reasoning_effort(effort: Option<&Value>) -> Option<String> {
    let effort = effort?.as_str()?.trim().to_lowercase();
    match effort.as_str() {
        "" | "none" => None,
        _ => Some(effort),
    }
}

fn text_blocks(content: Option<&Value>) -> Result<Vec<Value>, IngressError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    match content {
        Value::String(text) => Ok(text_block(text)),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                let part = part.as_object().ok_or(IngressError::NotJson)?;
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                        let block = text_block(text);
                        blocks.extend(block);
                    }
                    Some("image_url") => {
                        return Err(IngressError::UnsupportedPart("image_url".into()));
                    }
                    Some(other) => return Err(IngressError::UnsupportedPart(other.into())),
                    None => return Err(IngressError::NotJson),
                }
            }
            Ok(blocks)
        }
        _ => Err(IngressError::NotJson),
    }
}

fn text_block(text: &str) -> Vec<Value> {
    // Blank text is not a content block: it would create an empty turn the
    // Messages API rejects.
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": text })]
    }
}

fn content_blocks(content: Option<&Value>) -> Result<Vec<Value>, IngressError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    match content {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(text_block(text)),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                let part = part.as_object().ok_or(IngressError::NotJson)?;
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or(IngressError::NotJson)?;
                match kind {
                    "text" => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                        blocks.extend(text_block(text));
                    }
                    "image_url" => blocks.push(image_block(part)?),
                    other => return Err(IngressError::UnsupportedPart(other.into())),
                }
            }
            Ok(blocks)
        }
        _ => Err(IngressError::NotJson),
    }
}

/// `image_url.url` is either a `data:` URL (base64 inline) or an https URL,
/// which is exactly Anthropic's two image sources.
fn image_block(part: &Map<String, Value>) -> Result<Value, IngressError> {
    let url = part
        .get("image_url")
        .and_then(|image| image.get("url"))
        .and_then(Value::as_str)
        .ok_or_else(|| IngressError::UnsupportedPart("image_url".into()))?;
    let block = if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest
            .split_once(',')
            .ok_or_else(|| IngressError::UnsupportedPart("image_url".into()))?;
        if !meta.ends_with(";base64") {
            return Err(IngressError::UnsupportedPart("image_url".into()));
        }
        json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": meta.trim_end_matches(";base64"),
                "data": data,
            }
        })
    } else if url.starts_with("http://") || url.starts_with("https://") {
        json!({ "type": "image", "source": { "type": "url", "url": url } })
    } else {
        return Err(IngressError::UnsupportedPart("image_url".into()));
    };
    Ok(block)
}

fn tool_use_blocks(tool_calls: Option<&Value>) -> Result<Vec<Value>, IngressError> {
    let Some(tool_calls) = tool_calls else {
        return Ok(Vec::new());
    };
    let tool_calls = tool_calls.as_array().ok_or(IngressError::NotJson)?;
    let mut blocks = Vec::new();
    for call in tool_calls {
        let call = call.as_object().ok_or(IngressError::NotJson)?;
        if call.get("type").and_then(Value::as_str) != Some("function") {
            continue;
        }
        let function = call
            .get("function")
            .and_then(Value::as_object)
            .ok_or(IngressError::NotJson)?;
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| IngressError::UnsupportedPart("tool_calls.id".into()))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or(IngressError::NotJson)?;
        blocks.push(json!({
            "type": "tool_use",
            "id": id,
            "name": sanitize_tool_name(name),
            "input": tool_arguments(function.get("arguments")),
        }));
    }
    Ok(blocks)
}

/// OpenAI carries tool arguments as a JSON *string*; Anthropic wants the
/// object. Malformed or non-object arguments become `{}`, which is what the
/// harness would have sent for a call with no arguments.
fn tool_arguments(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(object)) => Value::Object(object),
            _ => json!({}),
        },
        Some(value @ Value::Object(_)) => value.clone(),
        _ => json!({}),
    }
}

fn tool_result_block(message: &Map<String, Value>) -> Result<Value, IngressError> {
    let id = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| IngressError::UnsupportedPart("tool_call_id".into()))?;
    let content = match message.get("content") {
        Some(Value::String(text)) => Value::String(text.clone()),
        Some(Value::Array(parts)) => Value::Array({
            let mut blocks = Vec::new();
            for part in parts {
                let part = part.as_object().ok_or(IngressError::NotJson)?;
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                        blocks.extend(text_block(text));
                    }
                    Some(other) => return Err(IngressError::UnsupportedPart(other.into())),
                    None => return Err(IngressError::NotJson),
                }
            }
            blocks
        }),
        Some(Value::Null) | None => Value::String(String::new()),
        _ => return Err(IngressError::NotJson),
    };
    Ok(json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": content,
    }))
}

fn tool_definitions(tools: Option<&Value>) -> Result<Vec<Value>, IngressError> {
    let Some(tools) = tools else {
        return Ok(Vec::new());
    };
    let tools = tools.as_array().ok_or(IngressError::NotJson)?;
    let mut definitions = Vec::new();
    for tool in tools {
        let tool = tool.as_object().ok_or(IngressError::NotJson)?;
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            continue;
        }
        let function = tool
            .get("function")
            .and_then(Value::as_object)
            .ok_or(IngressError::NotJson)?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or(IngressError::NotJson)?;
        let mut definition = Map::new();
        definition.insert("name".into(), json!(sanitize_tool_name(name)));
        definition.insert(
            "description".into(),
            json!(function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()),
        );
        definition.insert(
            "input_schema".into(),
            function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        );
        if let Some(strict) = function.get("strict").or_else(|| tool.get("strict")) {
            if let Some(strict) = strict.as_bool() {
                definition.insert("strict".into(), json!(strict));
            }
        }
        definitions.push(Value::Object(definition));
    }
    Ok(definitions)
}

fn tool_choice(choice: Option<&Value>) -> Option<Value> {
    let choice = choice?;
    match choice {
        Value::String(name) => match name.as_str() {
            "none" => Some(json!({ "type": "none" })),
            "auto" => Some(json!({ "type": "auto" })),
            "required" => Some(json!({ "type": "any" })),
            _ => None,
        },
        Value::Object(choice) => {
            let kind = choice.get("type").and_then(Value::as_str)?;
            match kind {
                "none" => Some(json!({ "type": "none" })),
                "auto" => Some(json!({ "type": "auto" })),
                "required" | "any" => Some(json!({ "type": "any" })),
                "function" => {
                    let name = choice
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .or_else(|| choice.get("name"))
                        .and_then(Value::as_str)?;
                    Some(json!({ "type": "tool", "name": sanitize_tool_name(name) }))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Anthropic tool names are `[a-zA-Z0-9_-]{1,64}`. Clients such as MCP
/// servers routinely hand over names with dots or spaces.
fn sanitize_tool_name(name: &str) -> String {
    let mut sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    sanitized.truncate(MAX_TOOL_NAME);
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(request: Value) -> Value {
        translate_chat_request(&serde_json::to_vec(&request).unwrap()).unwrap()
    }

    #[test]
    fn plain_user_message_becomes_one_text_turn() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [{"role": "user", "content": "hello"}],
        }));
        assert_eq!(out["model"], "claude-sonnet");
        assert_eq!(out["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(out["stream"], false);
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][0]["content"][0]["type"], "text");
        assert_eq!(out["messages"][0]["content"][0]["text"], "hello");
        assert!(out.get("system").is_none());
    }

    #[test]
    fn system_and_developer_messages_both_become_system_blocks() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "developer", "content": [{"type": "text", "text": "use tools"}]},
                {"role": "user", "content": "hi"},
            ],
        }));
        assert_eq!(out["system"][0]["text"], "be terse");
        assert_eq!(out["system"][1]["text"], "use tools");
        assert_eq!(out["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_and_results_coalesce() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [
                {"role": "user", "content": "read a.rs"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"},
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "fn main() {}"},
                {"role": "tool", "tool_call_id": "call_2", "content": "second"},
            ],
        }));
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[1]["content"][0]["input"]["path"], "a.rs");
        // Both tool results land in one user turn: Anthropic alternates roles.
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(messages[2]["content"][1]["tool_use_id"], "call_2");
    }

    #[test]
    fn malformed_tool_arguments_become_an_empty_object() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [
                {"role": "user", "content": "x"},
                {"role": "assistant", "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "noop", "arguments": "not json"},
                }]},
            ],
        }));
        assert_eq!(out["messages"][1]["content"][0]["input"], json!({}));
    }

    #[test]
    fn function_tools_and_required_choice_map_to_anthropic() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [{"role": "user", "content": "x"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "read a file",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
                    "strict": true,
                },
            }],
            "tool_choice": "required",
            "parallel_tool_calls": false,
        }));
        assert_eq!(out["tools"][0]["name"], "read_file");
        assert_eq!(
            out["tools"][0]["input_schema"]["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(out["tools"][0]["strict"], true);
        assert_eq!(out["tool_choice"]["type"], "any");
        assert_eq!(out["tool_choice"]["disable_parallel_tool_use"], true);
    }

    #[test]
    fn function_tool_choice_names_the_tool() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [{"role": "user", "content": "x"}],
            "tools": [{"type": "function", "function": {"name": "read.file"}}],
            "tool_choice": {"type": "function", "function": {"name": "read.file"}},
        }));
        assert_eq!(
            out["tool_choice"],
            json!({"type": "tool", "name": "read_file"})
        );
        assert_eq!(out["tools"][0]["name"], "read_file");
        assert_eq!(
            out["tools"][0]["input_schema"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn tool_choice_without_tools_is_not_forwarded() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [{"role": "user", "content": "x"}],
            "tool_choice": "required",
        }));
        assert!(out.get("tool_choice").is_none());
        assert!(out.get("tools").is_none());
    }

    #[test]
    fn inline_images_keep_their_data_url_and_remote_images_stay_urls() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "max_completion_tokens": 512,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}},
            ]}],
        }));
        assert_eq!(out["max_tokens"], 512);
        let content = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "AAAA");
        assert_eq!(content[2]["source"]["type"], "url");
        assert_eq!(content[2]["source"]["url"], "https://example.com/a.png");
    }

    #[test]
    fn sampling_parameters_and_stop_strings_are_carried() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [{"role": "user", "content": "x"}],
            "temperature": 0.2,
            "top_p": 0.9,
            "stop": "DONE",
            "reasoning_effort": "HIGH",
        }));
        assert_eq!(out["temperature"], 0.2);
        assert_eq!(out["top_p"], 0.9);
        assert_eq!(out["stop_sequences"], json!(["DONE"]));
        assert_eq!(out["output_config"]["effort"], "high");
    }

    #[test]
    fn effort_none_is_omitted_rather_than_forwarded() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [{"role": "user", "content": "x"}],
            "reasoning_effort": "none",
        }));
        assert!(out.get("output_config").is_none());
    }

    #[test]
    fn consecutive_same_role_messages_merge_into_one_turn() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [
                {"role": "user", "content": "a"},
                {"role": "user", "content": "b"},
            ],
        }));
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn system_only_request_gets_the_blank_user_turn_anthropic_requires() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [{"role": "system", "content": "instructions"}],
        }));
        assert_eq!(out["system"][0]["text"], "instructions");
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][0]["content"][0]["text"], "");
    }

    #[test]
    fn blank_content_does_not_create_an_empty_turn() {
        let out = translate(json!({
            "model": "claude-sonnet",
            "messages": [
                {"role": "user", "content": "  "},
                {"role": "assistant", "content": ""},
                {"role": "user", "content": "real"},
            ],
        }));
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"][0]["text"], "real");
    }

    #[test]
    fn errors_are_typed_and_name_the_offending_input() {
        assert_eq!(
            translate_chat_request(b"not json").unwrap_err(),
            IngressError::NotJson
        );
        assert_eq!(
            translate_chat_request(br#"{"messages":[]}"#).unwrap_err(),
            IngressError::MissingModel
        );
        assert_eq!(
            translate_chat_request(br#"{"model":"m"}"#).unwrap_err(),
            IngressError::EmptyMessages
        );
        assert_eq!(
            translate_chat_request(br#"{"model":"m","messages":[{"role":"user","content":[]}]}"#)
                .unwrap_err(),
            IngressError::EmptyMessages
        );
        let unsupported = translate_chat_request(
            br#"{"model":"m","messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"AAAA"}}]}]}"#,
        )
        .unwrap_err();
        assert_eq!(
            unsupported,
            IngressError::UnsupportedPart("input_audio".into())
        );
        assert!(unsupported.to_string().contains("input_audio"));
    }
}
