//! Gemini `generateContent` request → Anthropic Messages request translation.
//!
//! This is the *ingress* half of the Gemini client dialect: a caller that
//! speaks Google's `generateContent` wire format (the `gemini` CLI, a Gemini
//! SDK, or a remote `clud serve` instance in Gemini mode) is rewritten into the
//! Messages request the rest of clud already serves. It needs no provider row,
//! credential or route, so it is additive: nothing enables it until a route
//! selects it, and an unused module changes no behaviour.
//!
//! Ported from CLIProxyAPI
//! `internal/translator/claude/gemini/claude_gemini_request.go`
//! (`ConvertGeminiRequestToClaude`), plus the shared helpers it calls
//! (`translatorcommon.NewClaudeMessageAccumulator`, `util.SanitizeClaudeFunctionName`,
//! `normalizeClaudeToolSchema`, `lowercaseClaudeToolSchemaTypes`,
//! `setClaudeToolChoiceFromGeminiToolConfig`).
//!
//! Deliberate differences, all documented rather than silent:
//!
//! * **The system instruction is a top-level `system` array**, not a synthetic
//!   first `user` turn. Upstream has to fold it into `messages` because the
//!   Claude Code API shape it targets rejects a top-level field; clud speaks the
//!   Messages API, which owns `system`.
//! * **A `functionCall` in a non-assistant turn is a typed error**, not dropped:
//!   a model call can only be replayed from an assistant turn, and silently
//!   losing it would answer a truncated conversation.
//! * **Media Claude cannot read never becomes invisible.** In an assistant turn
//!   the block becomes the same placeholder text upstream emits, and in a user
//!   turn it is a typed error naming the part (upstream records a turn drop and
//!   continues), so the caller learns its attachment did not arrive.
//! * **`candidateCount > 1` is a typed error**: clud serves one response per
//!   request and will not silently answer a one-of-N question with one.
//! * **No `metadata.user_id` derivation and no thinking-summary rewriting.**
//!   clud does not fingerprint callers (see the same note in `openai_ingress`).
//! * **`service_tier` is not forwarded**; clud's catalog does not carry a tier
//!   per route and forwarding an unknown tier is a validation error upstream.

use crate::provider_catalog::EffortLevel;
use serde_json::{json, Value};

/// Why a `generateContent` request cannot be served as a Messages request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiClientIngressError {
    /// The body was not a JSON object.
    NotJson,
    /// No model in the request and no override, so the target is unknown.
    MissingModel,
    /// The request carried no `contents` to send.
    EmptyContents,
    /// A content part clud has no Messages spelling for. Names the part.
    UnsupportedPart(String),
    /// A generation option clud cannot honour. Names the option.
    UnsupportedOption(String),
}

impl std::fmt::Display for GeminiClientIngressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson => write!(formatter, "the request body is not a JSON object"),
            Self::MissingModel => write!(formatter, "the request names no model"),
            Self::EmptyContents => write!(formatter, "the request carries no contents"),
            Self::UnsupportedPart(part) => {
                write!(
                    formatter,
                    "cannot translate {part} to a Messages content block"
                )
            }
            Self::UnsupportedOption(option) => {
                write!(formatter, "cannot honour {option} on a Messages route")
            }
        }
    }
}

impl std::error::Error for GeminiClientIngressError {}

/// Translate one Gemini `generateContent` request into an Anthropic Messages
/// request body.
///
/// `model` is the resolved Claude-side model id. When it is empty the request's
/// own `model` field is used, so a caller that already names a routable model
/// needs no override. `stream` is the caller's framing choice; a Gemini body
/// does not carry it (`streamGenerateContent` is a different URL).
pub fn translate_generate_content_request(
    model: &str,
    stream: bool,
    body: &[u8],
) -> Result<Value, GeminiClientIngressError> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|_| GeminiClientIngressError::NotJson)?;
    let request = parsed
        .as_object()
        .ok_or(GeminiClientIngressError::NotJson)?;

    let target = if model.trim().is_empty() {
        request
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or(GeminiClientIngressError::MissingModel)?
    } else {
        model.trim()
    };

    let mut out = json!({
        "model": target,
        "max_tokens": crate::openai_ingress::DEFAULT_MAX_TOKENS,
        "messages": [],
    });

    apply_generation_config(request, &mut out)?;
    apply_system_instruction(request, &mut out);

    let mut messages = build_messages(request)?;
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": [text_block("")] }));
    } else if messages[0].get("role").and_then(Value::as_str) == Some("assistant") {
        // The Messages API needs a user turn first; a conversation that opens
        // with a replayed model turn gets an empty user turn, exactly as the
        // OpenAI ingress does for a system-only request.
        messages.insert(0, json!({ "role": "user", "content": [text_block("")] }));
    }
    out["messages"] = Value::Array(messages);

    apply_tools(request, &mut out)?;
    apply_tool_config(request, &mut out);
    out["stream"] = Value::Bool(stream);
    Ok(out)
}

/// `clud __gemini-client-translate` — the process seam the Python suite drives.
/// Reads one `generateContent` request from stdin and writes the translated
/// Messages body (compact JSON + newline) to stdout. No session, backend,
/// credential or daemon action.
pub fn run_translate_stdin(model: Option<&str>, stream: bool) -> i32 {
    use std::io::{Read, Write};

    let mut body = Vec::new();
    if std::io::stdin().read_to_end(&mut body).is_err() {
        eprintln!("could not read the request body from stdin");
        return 2;
    }
    match translate_generate_content_request(model.unwrap_or_default(), stream, &body) {
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

fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// `generationConfig` → sampling, stop sequences and thinking.
fn apply_generation_config(
    request: &serde_json::Map<String, Value>,
    out: &mut Value,
) -> Result<(), GeminiClientIngressError> {
    let Some(config) = request.get("generationConfig").and_then(Value::as_object) else {
        return Ok(());
    };

    if let Some(max) = config.get("maxOutputTokens").and_then(Value::as_u64) {
        if max > 0 {
            out["max_tokens"] = json!(max);
        }
    }
    if let Some(temperature) = config.get("temperature").and_then(Value::as_f64) {
        out["temperature"] = json!(temperature);
    }
    if let Some(top_p) = config.get("topP").and_then(Value::as_f64) {
        out["top_p"] = json!(top_p);
    }
    if let Some(top_k) = config.get("topK").and_then(Value::as_i64) {
        if top_k > 0 {
            out["top_k"] = json!(top_k);
        }
    }
    if let Some(count) = config.get("candidateCount").and_then(Value::as_i64) {
        if count > 1 {
            return Err(GeminiClientIngressError::UnsupportedOption(
                "generationConfig.candidateCount > 1".to_string(),
            ));
        }
    }
    if let Some(stops) = config.get("stopSequences").and_then(Value::as_array) {
        let sequences: Vec<Value> = stops
            .iter()
            .filter_map(Value::as_str)
            .map(|stop| json!(stop))
            .collect();
        if !sequences.is_empty() {
            out["stop_sequences"] = Value::Array(sequences);
        }
    }
    if let Some(thinking) = config.get("thinkingConfig").and_then(Value::as_object) {
        apply_thinking_config(thinking, out)?;
    }
    Ok(())
}

fn apply_thinking_config(
    thinking: &serde_json::Map<String, Value>,
    out: &mut Value,
) -> Result<(), GeminiClientIngressError> {
    let level = thinking
        .get("thinkingLevel")
        .or_else(|| thinking.get("thinking_level"))
        .and_then(Value::as_str);
    if let Some(level) = level.map(str::trim).filter(|level| !level.is_empty()) {
        let level = if level.eq_ignore_ascii_case("minimal") {
            "low"
        } else {
            level
        };
        if level.eq_ignore_ascii_case("none") {
            out["thinking"] = json!({ "type": "disabled" });
        } else if level.eq_ignore_ascii_case("auto") {
            out["thinking"] = json!({ "type": "enabled" });
        } else {
            let parsed = EffortLevel::parse(level).ok_or_else(|| {
                GeminiClientIngressError::UnsupportedOption(format!(
                    "generationConfig.thinkingConfig.thinkingLevel {level}"
                ))
            })?;
            out["thinking"] = json!({ "type": "enabled" });
            out["output_config"] = json!({ "effort": parsed.as_str() });
        }
        return Ok(());
    }

    let budget = thinking
        .get("thinkingBudget")
        .or_else(|| thinking.get("thinking_budget"))
        .and_then(Value::as_i64);
    match budget {
        None => Ok(()),
        Some(0) => {
            out["thinking"] = json!({ "type": "disabled" });
            Ok(())
        }
        // -1 asks the provider to size the budget; the Messages API says that
        // with `enabled` and no `budget_tokens`.
        Some(budget) if budget < 0 => {
            out["thinking"] = json!({ "type": "enabled" });
            Ok(())
        }
        Some(budget) => {
            out["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            Ok(())
        }
    }
}

/// `systemInstruction` (or `system_instruction`) text parts → `system` blocks.
fn apply_system_instruction(request: &serde_json::Map<String, Value>, out: &mut Value) {
    let Some(instruction) = request
        .get("systemInstruction")
        .or_else(|| request.get("system_instruction"))
    else {
        return;
    };
    let Some(parts) = instruction.get("parts").and_then(Value::as_array) else {
        return;
    };
    let mut joined: Vec<&str> = Vec::new();
    for part in parts {
        if is_thought_part(part) {
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            joined.push(text);
        }
    }
    if joined.is_empty() {
        return;
    }
    out["system"] = json!([text_block(&joined.join("\n"))]);
}

/// `contents` → `messages`, merging adjacent turns of the same role.
fn build_messages(
    request: &serde_json::Map<String, Value>,
) -> Result<Vec<Value>, GeminiClientIngressError> {
    let Some(contents) = request.get("contents").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut messages: Vec<Value> = Vec::new();
    let mut pending_tool_ids: Vec<String> = Vec::new();
    let mut tool_call_counter: u64 = 0;

    for content in contents {
        let role = match content.get("role").and_then(Value::as_str) {
            Some("model") => "assistant",
            Some("function") | Some("tool") => "user",
            Some("user") | None | Some("") => "user",
            Some(other) => {
                return Err(GeminiClientIngressError::UnsupportedPart(format!(
                    "content role {other}"
                )))
            }
        };

        let mut blocks: Vec<Value> = Vec::new();
        if let Some(parts) = content.get("parts").and_then(Value::as_array) {
            for part in parts {
                if is_thought_part(part) {
                    continue;
                }
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    blocks.push(text_block(text));
                    continue;
                }
                if let Some(call) = part.get("functionCall") {
                    let block =
                        tool_use_block(call, role, &mut pending_tool_ids, &mut tool_call_counter)?;
                    blocks.push(block);
                    continue;
                }
                if let Some(response) = part.get("functionResponse") {
                    blocks.push(tool_result_block(
                        response,
                        &mut pending_tool_ids,
                        &mut tool_call_counter,
                    ));
                    continue;
                }
                if let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data")) {
                    blocks.push(inline_data_block(inline, role)?);
                    continue;
                }
                if let Some(file) = part.get("fileData").or_else(|| part.get("file_data")) {
                    blocks.push(file_data_block(file));
                    continue;
                }
                if part.as_object().is_some_and(|object| object.is_empty()) {
                    continue;
                }
                let named = part
                    .as_object()
                    .and_then(|object| object.keys().next())
                    .cloned()
                    .unwrap_or_else(|| "an empty part".to_string());
                return Err(GeminiClientIngressError::UnsupportedPart(format!(
                    "content part {named}"
                )));
            }
        }

        if blocks.is_empty() {
            continue;
        }

        let merged = match messages.last_mut() {
            Some(last) if last.get("role").and_then(Value::as_str) == Some(role) => last,
            _ => {
                messages.push(json!({ "role": role, "content": [] }));
                messages.last_mut().expect("a message was just pushed")
            }
        };
        if let Some(existing) = merged.get_mut("content").and_then(Value::as_array_mut) {
            existing.extend(blocks);
        }
    }

    if !contents.is_empty() && messages.is_empty() {
        return Err(GeminiClientIngressError::EmptyContents);
    }
    Ok(messages)
}

/// Gemini marks replayed reasoning with `thought: true`; the Messages ingress
/// does not replay thinking, so those parts are skipped (never errors).
fn is_thought_part(part: &Value) -> bool {
    part.get("thought")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn tool_id_from(value: &Value) -> Option<String> {
    ["id", "call_id"]
        .iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|id| !id.is_empty())
        .map(str::to_string)
}

fn next_generated_tool_id(counter: &mut u64) -> String {
    *counter += 1;
    format!("toolu_gemini_{:016}", *counter)
}

fn tool_use_block(
    call: &Value,
    role: &str,
    pending_tool_ids: &mut Vec<String>,
    counter: &mut u64,
) -> Result<Value, GeminiClientIngressError> {
    if role != "assistant" {
        return Err(GeminiClientIngressError::UnsupportedPart(
            "functionCall in a user turn".to_string(),
        ));
    }
    let tool_id = tool_id_from(call).unwrap_or_else(|| next_generated_tool_id(counter));
    pending_tool_ids.push(tool_id.clone());

    let name = call
        .get("name")
        .and_then(Value::as_str)
        .map(crate::openai_ingress::sanitize_tool_name)
        .unwrap_or_default();
    let input = call
        .get("args")
        .filter(|args| args.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(json!({
        "type": "tool_use",
        "id": tool_id,
        "name": name,
        "input": input,
    }))
}

fn tool_result_block(
    response: &Value,
    pending_tool_ids: &mut Vec<String>,
    counter: &mut u64,
) -> Value {
    let tool_use_id = match tool_id_from(response) {
        Some(custom) => {
            pending_tool_ids.retain(|pending| pending != &custom);
            custom
        }
        None if !pending_tool_ids.is_empty() => pending_tool_ids.remove(0),
        None => next_generated_tool_id(counter),
    };
    let content = match response.get("response") {
        Some(response) => match response.get("result") {
            Some(result) => result
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| result.to_string()),
            None => response.to_string(),
        },
        None => String::new(),
    };
    json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": content,
    })
}

fn mime_type(value: &Value) -> Option<&str> {
    ["mimeType", "mime_type"]
        .iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|mime| !mime.is_empty())
}

fn inline_data_block(inline: &Value, role: &str) -> Result<Value, GeminiClientIngressError> {
    let mime = mime_type(inline).unwrap_or_default();
    let data = inline
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if mime.is_empty() || data.is_empty() {
        return Err(GeminiClientIngressError::UnsupportedPart(
            "an inlineData part with no mimeType or data".to_string(),
        ));
    }
    let lower = mime.to_ascii_lowercase();
    if lower.starts_with("image/") {
        return Ok(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": mime, "data": data },
        }));
    }
    if lower.starts_with("application/") || lower.starts_with("text/") {
        return Ok(json!({
            "type": "document",
            "source": { "type": "base64", "media_type": mime, "data": data },
        }));
    }
    if role == "assistant" {
        // A model turn cannot be silently shortened; the placeholder keeps the
        // transcript shape without claiming Claude can read the media.
        return Ok(text_block(&format!(
            "Media content: inline data (Type: {mime})"
        )));
    }
    Err(GeminiClientIngressError::UnsupportedPart(format!(
        "inlineData with mimeType {mime}"
    )))
}

fn file_data_block(file: &Value) -> Value {
    let uri = ["fileUri", "file_uri"]
        .iter()
        .filter_map(|key| file.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|uri| !uri.is_empty())
        .unwrap_or_default();
    let mime = mime_type(file).unwrap_or_default();
    let lower = mime.to_ascii_lowercase();
    if lower.starts_with("image/") {
        return json!({ "type": "image", "source": { "type": "url", "url": uri } });
    }
    if lower.starts_with("application/") || lower.starts_with("text/") {
        let mut source = json!({ "type": "url", "url": uri });
        if !mime.is_empty() {
            source["media_type"] = json!(mime);
        }
        return json!({ "type": "document", "source": source });
    }
    let mut info = format!("File: {uri}");
    if !mime.is_empty() {
        info.push_str(&format!(" (Type: {mime})"));
    }
    text_block(&info)
}

/// `tools[].functionDeclarations[]` → Messages `tools`.
fn apply_tools(
    request: &serde_json::Map<String, Value>,
    out: &mut Value,
) -> Result<(), GeminiClientIngressError> {
    let Some(tools) = request.get("tools").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut declared: Vec<Value> = Vec::new();
    for tool in tools {
        let Some(functions) = tool.get("functionDeclarations").and_then(Value::as_array) else {
            continue;
        };
        for function in functions {
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .map(crate::openai_ingress::sanitize_tool_name)
                .unwrap_or_default();
            if name.is_empty() {
                return Err(GeminiClientIngressError::UnsupportedPart(
                    "a functionDeclaration with no name".to_string(),
                ));
            }
            let schema = function
                .get("parametersJsonSchema")
                .or_else(|| function.get("parameters"))
                .map(normalize_tool_schema)
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            let mut declared_tool = json!({ "name": name, "input_schema": schema });
            if let Some(description) = function.get("description").and_then(Value::as_str) {
                declared_tool["description"] = json!(description);
            }
            declared.push(declared_tool);
        }
    }
    if !declared.is_empty() {
        out["tools"] = Value::Array(declared);
    }
    Ok(())
}

/// Claude validates draft-07 schemas and expects lowercase `type` values; Gemini
/// declarations carry neither guarantee.
fn normalize_tool_schema(parameters: &Value) -> Value {
    if !parameters.is_object() {
        return json!({ "type": "object", "properties": {} });
    }
    let mut schema = parameters.clone();
    if schema.get("additionalProperties").and_then(Value::as_bool) != Some(false) {
        schema["additionalProperties"] = json!(false);
    }
    schema["$schema"] = json!("http://json-schema.org/draft-07/schema#");
    lowercase_schema_types(&mut schema);
    schema
}

fn lowercase_schema_types(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(kind)) = object.get("type") {
                object.insert("type".to_string(), json!(kind.to_ascii_lowercase()));
            }
            for child in object.values_mut() {
                lowercase_schema_types(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                lowercase_schema_types(item);
            }
        }
        _ => {}
    }
}

/// `toolConfig.functionCallingConfig` → `tool_choice`.
fn apply_tool_config(request: &serde_json::Map<String, Value>, out: &mut Value) {
    let Some(config) = request
        .get("toolConfig")
        .or_else(|| request.get("tool_config"))
    else {
        return;
    };
    let Some(calling) = config
        .get("functionCallingConfig")
        .or_else(|| config.get("function_calling_config"))
    else {
        return;
    };
    let Some(mode) = calling.get("mode").and_then(Value::as_str) else {
        return;
    };
    match mode.to_ascii_uppercase().as_str() {
        "AUTO" => out["tool_choice"] = json!({ "type": "auto" }),
        "NONE" => out["tool_choice"] = json!({ "type": "none" }),
        "ANY" => {
            let allowed = calling
                .get("allowedFunctionNames")
                .or_else(|| calling.get("allowed_function_names"))
                .and_then(Value::as_array);
            match allowed {
                Some(names) if names.len() == 1 => {
                    let name = names[0]
                        .as_str()
                        .map(crate::openai_ingress::sanitize_tool_name)
                        .unwrap_or_default();
                    out["tool_choice"] = json!({ "type": "tool", "name": name });
                }
                _ => out["tool_choice"] = json!({ "type": "any" }),
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(body: Value) -> Value {
        translate_generate_content_request("claude-sonnet-4", false, body.to_string().as_bytes())
            .expect("the request translates")
    }

    #[test]
    fn system_instruction_and_text_turns_map_to_messages() {
        let out = translate(json!({
            "systemInstruction": { "parts": [{ "text": "be brief" }, { "text": "no lists" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "hi" }] },
                { "role": "model", "parts": [{ "text": "hello" }] },
            ],
        }));
        assert_eq!(out["model"], "claude-sonnet-4");
        assert_eq!(out["max_tokens"], crate::openai_ingress::DEFAULT_MAX_TOKENS);
        assert_eq!(out["system"][0]["text"], "be brief\nno lists");
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][0]["content"][0]["text"], "hi");
        assert_eq!(out["messages"][1]["role"], "assistant");
        assert_eq!(out["messages"][1]["content"][0]["text"], "hello");
        assert_eq!(out["stream"], false);
    }

    #[test]
    fn adjacent_same_role_turns_merge_into_one_message() {
        let out = translate(json!({
            "contents": [
                { "role": "user", "parts": [{ "text": "one" }] },
                { "role": "user", "parts": [{ "text": "two" }] },
            ],
        }));
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(messages[0]["content"][1]["text"], "two");
    }

    #[test]
    fn function_calls_pair_with_results_by_fifo_and_keep_given_ids() {
        let out = translate(json!({
            "contents": [
                { "role": "user", "parts": [{ "text": "run it" }] },
                { "role": "model", "parts": [
                    { "functionCall": { "name": "Read.File", "args": { "path": "a" } } },
                    { "functionCall": { "name": "list", "id": "call_7", "args": {} } },
                ] },
                { "role": "user", "parts": [
                    { "functionResponse": { "name": "Read.File", "response": { "result": "ok" } } },
                    { "functionResponse": { "name": "list", "response": { "items": [1] } } },
                ] },
            ],
        }));
        let assistant = &out["messages"][1];
        let first = &assistant["content"][0];
        let second = &assistant["content"][1];
        assert_eq!(first["type"], "tool_use");
        assert_eq!(first["name"], "Read_File");
        assert_eq!(first["id"], "toolu_gemini_0000000000000001");
        assert_eq!(first["input"]["path"], "a");
        assert_eq!(second["id"], "call_7");

        let results = &out["messages"][2]["content"];
        assert_eq!(results[0]["tool_use_id"], "toolu_gemini_0000000000000001");
        assert_eq!(results[0]["content"], "ok");
        assert_eq!(results[1]["tool_use_id"], "call_7");
        assert_eq!(results[1]["content"], "{\"items\":[1]}");
    }

    #[test]
    fn inline_image_becomes_a_base64_image_block_and_documents_become_documents() {
        let out = translate(json!({
            "contents": [
                { "role": "user", "parts": [
                    { "inlineData": { "mimeType": "image/png", "data": "AAA" } },
                    { "inline_data": { "mime_type": "application/pdf", "data": "BBB" } },
                ] },
            ],
        }));
        let blocks = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], "AAA");
        assert_eq!(blocks[1]["type"], "document");
        assert_eq!(blocks[1]["source"]["media_type"], "application/pdf");
    }

    #[test]
    fn unreadable_user_media_is_a_named_error_but_an_assistant_turn_gets_a_placeholder() {
        let err = translate_generate_content_request(
            "claude-sonnet-4",
            false,
            json!({
                "contents": [{ "role": "user", "parts": [
                    { "inlineData": { "mimeType": "video/mp4", "data": "AAA" } },
                ] }],
            })
            .to_string()
            .as_bytes(),
        )
        .expect_err("video has no Messages spelling");
        assert_eq!(
            err,
            GeminiClientIngressError::UnsupportedPart("inlineData with mimeType video/mp4".into())
        );

        let out = translate(json!({
            "contents": [{ "role": "model", "parts": [
                { "inlineData": { "mimeType": "video/mp4", "data": "AAA" } },
            ] }],
        }));
        assert_eq!(out["messages"][1]["content"][0]["type"], "text");
        assert_eq!(
            out["messages"][1]["content"][0]["text"],
            "Media content: inline data (Type: video/mp4)"
        );
    }

    #[test]
    fn file_data_becomes_a_url_source() {
        let out = translate(json!({
            "contents": [{ "role": "user", "parts": [
                { "fileData": { "mimeType": "image/jpeg", "fileUri": "https://x/y.jpg" } },
            ] }],
        }));
        let block = &out["messages"][0]["content"][0];
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "url");
        assert_eq!(block["source"]["url"], "https://x/y.jpg");
    }

    #[test]
    fn function_declarations_become_tools_with_a_lowercase_closed_schema() {
        let out = translate(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "tools": [{ "functionDeclarations": [
                {
                    "name": "Read.File",
                    "description": "read",
                    "parameters": {
                        "type": "OBJECT",
                        "properties": { "path": { "type": "STRING" } },
                        "required": ["path"],
                    },
                },
                { "name": "noargs" },
            ] }],
        }));
        let tools = out["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "Read_File");
        assert_eq!(tools[0]["description"], "read");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
        assert_eq!(
            tools[0]["input_schema"]["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(tools[0]["input_schema"]["additionalProperties"], false);
        assert_eq!(
            tools[0]["input_schema"]["$schema"],
            "http://json-schema.org/draft-07/schema#"
        );
        assert_eq!(tools[1]["input_schema"]["type"], "object");
    }

    #[test]
    fn tool_config_modes_map_to_tool_choice() {
        let any_one = translate(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "toolConfig": { "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": ["Read.File"],
            } },
        }));
        assert_eq!(any_one["tool_choice"]["type"], "tool");
        assert_eq!(any_one["tool_choice"]["name"], "Read_File");

        let auto = translate(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "tool_config": { "function_calling_config": { "mode": "AUTO" } },
        }));
        assert_eq!(auto["tool_choice"]["type"], "auto");
    }

    #[test]
    fn thinking_budget_and_level_both_map_to_anthropic_thinking() {
        let budget = translate(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "generationConfig": { "thinkingConfig": { "thinkingBudget": 4096 } },
        }));
        assert_eq!(budget["thinking"]["type"], "enabled");
        assert_eq!(budget["thinking"]["budget_tokens"], 4096);

        let level = translate(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "generationConfig": { "thinkingConfig": { "thinkingLevel": "high" } },
        }));
        assert_eq!(level["thinking"]["type"], "enabled");
        assert_eq!(level["output_config"]["effort"], "high");

        let none = translate(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "generationConfig": { "thinkingConfig": { "thinkingLevel": "none" } },
        }));
        assert_eq!(none["thinking"]["type"], "disabled");
        assert!(none.get("output_config").is_none());
    }

    #[test]
    fn sampling_and_stop_sequences_are_forwarded() {
        let out = translate(json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "generationConfig": {
                "maxOutputTokens": 512,
                "temperature": 0.25,
                "topP": 0.9,
                "topK": 40,
                "stopSequences": ["STOP"],
            },
        }));
        assert_eq!(out["max_tokens"], 512);
        assert_eq!(out["temperature"], 0.25);
        assert_eq!(out["top_p"], 0.9);
        assert_eq!(out["top_k"], 40);
        assert_eq!(out["stop_sequences"][0], "STOP");
    }

    #[test]
    fn a_multi_candidate_request_is_refused_by_name() {
        let err = translate_generate_content_request(
            "claude-sonnet-4",
            false,
            json!({
                "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
                "generationConfig": { "candidateCount": 3 },
            })
            .to_string()
            .as_bytes(),
        )
        .expect_err("one response per request");
        assert_eq!(
            err,
            GeminiClientIngressError::UnsupportedOption(
                "generationConfig.candidateCount > 1".into()
            )
        );
    }

    #[test]
    fn thought_parts_are_skipped_and_a_model_first_turn_gets_a_blank_user_turn() {
        let out = translate(json!({
            "contents": [
                { "role": "model", "parts": [
                    { "text": "prior" },
                    { "text": "reasoning", "thought": true },
                ] },
                { "role": "user", "parts": [{ "text": "next" }] },
            ],
        }));
        let messages = out["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "");
        assert_eq!(messages[1]["content"].as_array().unwrap().len(), 1);
        assert_eq!(messages[1]["content"][0]["text"], "prior");
    }

    #[test]
    fn a_model_override_wins_and_a_missing_model_is_an_error() {
        let overridden = translate_generate_content_request(
            "claude-opus-4",
            true,
            json!({ "model": "gemini-3-pro", "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(overridden["model"], "claude-opus-4");
        assert_eq!(overridden["stream"], true);

        let err = translate_generate_content_request(
            "",
            false,
            json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] })
                .to_string()
                .as_bytes(),
        )
        .expect_err("no model");
        assert_eq!(err, GeminiClientIngressError::MissingModel);
    }

    #[test]
    fn contents_without_a_sendable_part_are_refused_rather_than_sent_empty() {
        let err = translate_generate_content_request(
            "claude-sonnet-4",
            false,
            json!({ "contents": [{ "role": "user", "parts": [{ "text": "x", "thought": true }] }] })
                .to_string()
                .as_bytes(),
        )
        .expect_err("nothing to send");
        assert_eq!(err, GeminiClientIngressError::EmptyContents);
    }
}
