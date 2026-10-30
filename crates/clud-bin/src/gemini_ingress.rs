//! Anthropic Messages → Gemini `generateContent` request translation.
//!
//! This is the *egress* half of Gemini-protocol support: a Messages request the
//! rest of clud already speaks is rewritten into the body Google's
//! `generateContent` endpoint expects, so none of the routing, failover,
//! catalog or credential machinery has to learn a second protocol until a
//! Gemini upstream client exists.
//!
//! Ported from CLIProxyAPI
//! `internal/translator/gemini/claude/gemini_claude_request.go`
//! (`ConvertClaudeRequestToGemini`), plus its shared helpers
//! `internal/translator/common/{gemini,claude_system,parts}.go` and the default
//! safety list in `internal/translator/gemini/common/safety.go`.
//!
//! Deliberate differences, all documented rather than silent:
//!
//! * **`max_tokens` is honoured** (`generationConfig.maxOutputTokens`).
//!   Upstream drops it entirely; a Messages request always carries it, and
//!   silently ignoring a caller's cap is the kind of hidden rewrite clud avoids.
//! * **Unsupported content parts are a typed error naming the part**, not a
//!   silent drop. Upstream drops a `url`/`file` image source and any unknown
//!   block type; clud tells the caller its input did not arrive.
//! * **No `thought` part on the way in.** Upstream can replay an assistant
//!   `thinking` block only because it owns Gemini's thought-signature store.
//!   clud has no such store, and emitting a thought part without a valid
//!   signature makes Gemini reject the turn, so thinking history is dropped.
//! * **No Claude Code attribution stripping and no signature compat shims.**
//!   clud forwards what the caller sent and does not fingerprint requests.
//! * **`adaptive` thinking without an explicit effort becomes
//!   `thinkingLevel: high`.** Upstream asks its model registry for the target's
//!   maximum budget; clud has no Gemini catalog row yet (that is a separate
//!   slice), so it states the level instead of guessing a numeric budget.
//! * **JSON-Schema normalization is not ported.** `input_schema` is forwarded
//!   as `parametersJsonSchema` verbatim; upstream's `CleanJSONSchemaForGemini`
//!   (local `$ref` inlining, union flattening, enum/const coercions) is a named
//!   follow-up slice, not something to half-copy here.
//!
//! Nothing here changes the launch-scoped gateway contract: there is no route
//! and no lifecycle change, only a pure translator plus its hidden process seam.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

/// Upstream's default safety configuration. Gemini applies its own defaults
/// when the field is absent, and those are stricter than what a coding harness
/// needs, so the explicit list is part of the ported body.
const SAFETY_SETTINGS: [(&str, &str); 5] = [
    ("HARM_CATEGORY_HARASSMENT", "OFF"),
    ("HARM_CATEGORY_HATE_SPEECH", "OFF"),
    ("HARM_CATEGORY_SEXUALLY_EXPLICIT", "OFF"),
    ("HARM_CATEGORY_DANGEROUS_CONTENT", "OFF"),
    ("HARM_CATEGORY_CIVIC_INTEGRITY", "BLOCK_NONE"),
];

/// `clud __claude-gemini-translate` — the process seam behind the Python
/// subprocess test. Reads one Anthropic Messages body from stdin and writes the
/// translated `generateContent` body (compact JSON + newline) to stdout.
///
/// `model` overrides the target Gemini model id; without it the request's own
/// `model` is used. The seam is hidden and takes no session, backend,
/// credential or daemon action, so exercising the translator never starts a
/// harness.
pub fn run_translate_stdin(model: Option<&str>) -> i32 {
    use std::io::{Read, Write};

    let mut body = Vec::new();
    if std::io::stdin().read_to_end(&mut body).is_err() {
        eprintln!("could not read the request body from stdin");
        return 2;
    }
    match translate_messages_request(model.unwrap_or_default(), &body) {
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

/// Why a Messages request cannot be served as a `generateContent` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiIngressError {
    /// The body was not a JSON object.
    NotJson,
    /// No `model` string and no `--model` override, so the target is unknown.
    MissingModel,
    /// No usable `messages` array.
    EmptyMessages,
    /// A content part clud has no Gemini spelling for. Names the part.
    UnsupportedPart(String),
}

impl std::fmt::Display for GeminiIngressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson => write!(formatter, "the request body is not a JSON object"),
            Self::MissingModel => write!(formatter, "the request names no model"),
            Self::EmptyMessages => write!(formatter, "the request carries no messages"),
            Self::UnsupportedPart(part) => {
                write!(formatter, "cannot translate {part} to a Gemini part")
            }
        }
    }
}

impl std::error::Error for GeminiIngressError {}

/// Translate one Anthropic Messages request into a Gemini `generateContent`
/// request body.
///
/// `model` is the target Gemini model id; when empty the request's own `model`
/// is kept (upstream takes the resolved model name the same way).
pub fn translate_messages_request(model: &str, body: &[u8]) -> Result<Value, GeminiIngressError> {
    let parsed: Value = serde_json::from_slice(body).map_err(|_| GeminiIngressError::NotJson)?;
    let request = parsed.as_object().ok_or(GeminiIngressError::NotJson)?;

    let requested_model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target_model = if model.is_empty() {
        requested_model
    } else {
        model
    };
    if target_model.is_empty() {
        return Err(GeminiIngressError::MissingModel);
    }
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or(GeminiIngressError::EmptyMessages)?;

    let mut translation = Translation::default();
    let contents = translation.contents(messages)?;

    let mut out = json!({ "model": target_model, "contents": contents });
    if let Some(instruction) = system_instruction(request.get("system")) {
        out["systemInstruction"] = instruction;
    }

    let (declarations, has_strict_tool) = tool_declarations(request.get("tools"));
    if let Some(declarations) = declarations {
        out["tools"] = json!([{ "functionDeclarations": declarations }]);
    }
    if let Some(config) = tool_config(
        request.get("tool_choice"),
        has_strict_tool && out.get("tools").is_some(),
    ) {
        out["toolConfig"] = config;
    }

    let generation = generation_config(request);
    if let Some(generation) = generation.as_object().filter(|map| !map.is_empty()) {
        out["generationConfig"] = Value::Object(generation.clone());
    }

    out["safetySettings"] = safety_settings();
    Ok(out)
}

fn safety_settings() -> Value {
    Value::Array(
        SAFETY_SETTINGS
            .iter()
            .map(|(category, threshold)| json!({ "category": category, "threshold": threshold }))
            .collect(),
    )
}

/// `system` is a string or an array of text blocks in the Messages request.
fn system_instruction(system: Option<&Value>) -> Option<Value> {
    let parts: Vec<Value> = match system? {
        Value::String(text) => text_parts(std::iter::once(text.as_str())),
        Value::Array(blocks) => text_parts(blocks.iter().filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })),
        _ => Vec::new(),
    };
    if parts.is_empty() {
        return None;
    }
    Some(json!({ "role": "user", "parts": parts }))
}

fn text_parts<'a>(texts: impl Iterator<Item = &'a str>) -> Vec<Value> {
    texts
        .filter(|text| !text.trim().is_empty())
        .map(|text| json!({ "text": text }))
        .collect()
}

/// Translators carry the tool-name map and the tool_use ids a following user
/// turn has to answer, which is what makes deterministic `tool_result` order
/// possible.
#[derive(Default)]
struct Translation {
    tool_names: HashMap<String, String>,
    pending_tool_ids: Vec<String>,
}

impl Translation {
    fn contents(&mut self, messages: &[Value]) -> Result<Vec<Value>, GeminiIngressError> {
        let mut contents: Vec<Value> = Vec::new();
        for message in messages {
            let Some(original_role) = message.get("role").and_then(Value::as_str) else {
                continue;
            };
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            if matches!(original_role, "system" | "developer") {
                if let Some(reminder) = system_reminder(&content) {
                    contents.push(gemini_content("user", vec![json!({ "text": reminder })]));
                }
                continue;
            }
            let preceding = if original_role == "user" {
                std::mem::take(&mut self.pending_tool_ids)
            } else {
                Vec::new()
            };
            let content = if preceding.is_empty() {
                content
            } else {
                align_tool_results(&content, &preceding)
            };
            let mut parts = self.parts(original_role, &content)?;
            if original_role == "user" {
                parts = reorder_user_parts(parts);
            }
            if parts.is_empty() {
                continue;
            }
            let role = if original_role == "assistant" {
                "model"
            } else {
                "user"
            };
            contents.push(gemini_content(role, parts));
        }
        strip_unanswered_model_turn(&mut contents);
        Ok(merge_adjacent_user_turns(contents))
    }

    fn parts(
        &mut self,
        original_role: &str,
        content: &Value,
    ) -> Result<Vec<Value>, GeminiIngressError> {
        match content {
            Value::String(text) => Ok(text_parts(std::iter::once(text.as_str()))),
            Value::Array(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    self.push_block(original_role, block, &mut parts)?;
                }
                Ok(parts)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn push_block(
        &mut self,
        original_role: &str,
        block: &Value,
        parts: &mut Vec<Value>,
    ) -> Result<(), GeminiIngressError> {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                }
            }
            // Upstream replays these only with a signature it owns; see the
            // module doc.
            "thinking" => {}
            "tool_use" => self.push_tool_use(original_role, block, parts)?,
            "tool_result" => self.push_tool_result(block, parts)?,
            "image" | "document" | "container_upload" => match inline_data(block.get("source")) {
                Some(part) => parts.push(part),
                None => {
                    return Err(GeminiIngressError::UnsupportedPart(describe_part(
                        block, kind,
                    )))
                }
            },
            other => {
                return Err(GeminiIngressError::UnsupportedPart(describe_part(
                    block, other,
                )))
            }
        }
        Ok(())
    }

    fn push_tool_use(
        &mut self,
        original_role: &str,
        block: &Value,
        parts: &mut Vec<Value>,
    ) -> Result<(), GeminiIngressError> {
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
        let args = match block.get("input") {
            Some(Value::Object(map)) => Value::Object(map.clone()),
            _ => match block.get("input") {
                None | Some(Value::Null) => json!({}),
                Some(_) => {
                    return Err(GeminiIngressError::UnsupportedPart(
                        "a tool_use whose input is not a JSON object".to_string(),
                    ))
                }
            },
        };
        if !id.is_empty() && !name.is_empty() {
            self.tool_names.insert(id.to_string(), name.to_string());
        }
        let mut call = Map::new();
        call.insert("name".to_string(), json!(sanitize_tool_name(name)));
        call.insert("args".to_string(), args);
        if !id.is_empty() {
            call.insert("id".to_string(), json!(id));
        }
        parts.push(json!({ "functionCall": Value::Object(call) }));
        if original_role == "assistant" && !id.is_empty() {
            self.pending_tool_ids.push(id.to_string());
        }
        Ok(())
    }

    fn push_tool_result(
        &mut self,
        block: &Value,
        parts: &mut Vec<Value>,
    ) -> Result<(), GeminiIngressError> {
        let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
            return Ok(());
        };
        let name = self
            .tool_names
            .get(id)
            .cloned()
            .or_else(|| tool_name_from_id(id))
            .unwrap_or_else(|| id.to_string());
        let normalized = normalize_tool_result(block.get("content"));
        let mut response = Map::new();
        response.insert("id".to_string(), json!(id));
        response.insert("name".to_string(), json!(sanitize_tool_name(&name)));
        response.insert(
            "response".to_string(),
            json!({ "result": normalized.result }),
        );
        parts.push(json!({ "functionResponse": Value::Object(response) }));
        for image in normalized.images {
            parts.push(inline_data_part(&image.mime_type, &image.data));
        }
        Ok(())
    }
}

fn gemini_content(role: &str, parts: Vec<Value>) -> Value {
    json!({ "role": role, "parts": parts })
}

/// A mid-conversation `system`/`developer` message becomes an ordinary user
/// turn wrapped in the reminder envelope, so a non-Claude upstream treats it as
/// a directive rather than as user speech.
fn system_reminder(content: &Value) -> Option<String> {
    let texts: Vec<&str> = match content {
        Value::String(text) => vec![text.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        _ => Vec::new(),
    };
    let joined = texts
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.trim().is_empty() {
        return None;
    }
    Some(format!("<system-reminder>\n{joined}\n</system-reminder>"))
}

fn describes_base64_image(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("image")
        && block
            .get("source")
            .and_then(|source| source.get("type"))
            .and_then(Value::as_str)
            == Some("base64")
}

fn inline_data(source: Option<&Value>) -> Option<Value> {
    let source = source?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let mime_type = source.get("media_type").and_then(Value::as_str)?;
    let data = source.get("data").and_then(Value::as_str)?;
    if mime_type.is_empty() || data.is_empty() {
        return None;
    }
    Some(inline_data_part(mime_type, data))
}

fn inline_data_part(mime_type: &str, data: &str) -> Value {
    json!({ "inline_data": { "mime_type": mime_type, "data": data } })
}

fn describe_part(block: &Value, kind: &str) -> String {
    let kind = if kind.is_empty() { "an untyped" } else { kind };
    match block
        .get("source")
        .and_then(|source| source.get("type"))
        .and_then(Value::as_str)
    {
        Some(source) => format!("a {kind} content part whose source is `{source}`"),
        None => format!("a {kind} content part"),
    }
}

struct NormalizedToolResult {
    result: Value,
    images: Vec<Image>,
}

struct Image {
    mime_type: String,
    data: String,
}

fn image_from_block(block: &Value) -> Option<Image> {
    let source = block.get("source")?;
    Some(Image {
        mime_type: source
            .get("media_type")
            .and_then(Value::as_str)?
            .to_string(),
        data: source.get("data").and_then(Value::as_str)?.to_string(),
    })
}

/// Claude's `tool_result.content` may be a string, a block array, a single
/// object or absent. Gemini wants one `result` value plus any images as their
/// own parts (base64 blobs inside the textual result bloat the response).
fn normalize_tool_result(content: Option<&Value>) -> NormalizedToolResult {
    let empty = NormalizedToolResult {
        result: json!(""),
        images: Vec::new(),
    };
    match content {
        Some(Value::String(text)) => NormalizedToolResult {
            result: json!(text),
            images: Vec::new(),
        },
        Some(Value::Array(blocks)) => {
            let mut images = Vec::new();
            let mut kept: Vec<Value> = Vec::new();
            for block in blocks {
                if describes_base64_image(block) {
                    if let Some(image) = image_from_block(block) {
                        images.push(image);
                    }
                    continue;
                }
                kept.push(block.clone());
            }
            let result = match kept.len() {
                0 => json!(""),
                1 => kept.remove(0),
                _ => Value::Array(kept),
            };
            NormalizedToolResult { result, images }
        }
        Some(Value::Object(map)) => {
            if describes_base64_image(content.unwrap_or(&Value::Null)) {
                let images = image_from_block(content.unwrap_or(&Value::Null))
                    .map(|image| vec![image])
                    .unwrap_or_default();
                return NormalizedToolResult {
                    result: json!(""),
                    images,
                };
            }
            NormalizedToolResult {
                result: Value::Object(map.clone()),
                images: Vec::new(),
            }
        }
        Some(Value::Null) | None => empty,
        Some(other) => NormalizedToolResult {
            result: other.clone(),
            images: Vec::new(),
        },
    }
}

/// Tools arrive as `{name, description, input_schema}`; Gemini wants
/// `functionDeclarations` with `parametersJsonSchema`.
fn tool_declarations(tools: Option<&Value>) -> (Option<Vec<Value>>, bool) {
    let Some(tools) = tools.and_then(Value::as_array) else {
        return (None, false);
    };
    let mut declarations = Vec::new();
    let mut has_strict_tool = false;
    for tool in tools {
        if tool.get("strict").and_then(Value::as_bool) == Some(true) {
            has_strict_tool = true;
        }
        let Some(schema) = tool.get("input_schema").filter(|schema| schema.is_object()) else {
            continue;
        };
        let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
        let mut declaration = Map::new();
        declaration.insert("name".to_string(), json!(sanitize_tool_name(name)));
        if let Some(description) = tool.get("description").and_then(Value::as_str) {
            declaration.insert("description".to_string(), json!(description));
        }
        declaration.insert("parametersJsonSchema".to_string(), schema.clone());
        declarations.push(Value::Object(declaration));
    }
    if declarations.is_empty() {
        return (None, has_strict_tool);
    }
    (Some(declarations), has_strict_tool)
}

/// `auto | none | any | tool` → a Gemini function-calling mode. A strict tool
/// without an explicit choice asks Gemini to validate the call (`VALIDATED`),
/// which is upstream's behaviour.
fn tool_config(choice: Option<&Value>, has_strict_tool: bool) -> Option<Value> {
    let (kind, name) = match choice {
        Some(Value::Object(map)) => (
            map.get("type").and_then(Value::as_str).unwrap_or_default(),
            map.get("name").and_then(Value::as_str).unwrap_or_default(),
        ),
        Some(Value::String(kind)) => (kind.as_str(), ""),
        _ if has_strict_tool => ("strict", ""),
        _ => return None,
    };
    let mut calling = Map::new();
    match kind {
        "auto" => {
            calling.insert(
                "mode".to_string(),
                json!(if has_strict_tool { "VALIDATED" } else { "AUTO" }),
            );
        }
        "none" => {
            calling.insert("mode".to_string(), json!("NONE"));
        }
        "any" => {
            calling.insert("mode".to_string(), json!("ANY"));
        }
        "tool" => {
            calling.insert("mode".to_string(), json!("ANY"));
            if !name.is_empty() {
                calling.insert(
                    "allowedFunctionNames".to_string(),
                    json!([sanitize_tool_name(name)]),
                );
            }
        }
        "strict" => {
            calling.insert("mode".to_string(), json!("VALIDATED"));
        }
        _ => return None,
    }
    Some(json!({ "functionCallingConfig": Value::Object(calling) }))
}

fn generation_config(request: &Map<String, Value>) -> Value {
    let mut config = Map::new();
    copy_number(request, "max_tokens", "maxOutputTokens", &mut config);
    copy_number(request, "temperature", "temperature", &mut config);
    copy_number(request, "top_p", "topP", &mut config);
    copy_number(request, "top_k", "topK", &mut config);
    if let Some(thinking) = thinking_config(request) {
        config.insert("thinkingConfig".to_string(), thinking);
    }
    Value::Object(config)
}

fn copy_number(
    request: &Map<String, Value>,
    from: &str,
    to: &str,
    config: &mut Map<String, Value>,
) {
    if let Some(number) = request.get(from).and_then(Value::as_f64) {
        config.insert(to.to_string(), json!(number));
    }
}

fn thinking_config(request: &Map<String, Value>) -> Option<Value> {
    let thinking = request.get("thinking")?.as_object()?;
    match thinking.get("type").and_then(Value::as_str)? {
        "enabled" => {
            let budget = thinking.get("budget_tokens").and_then(Value::as_u64)?;
            Some(json!({ "thinkingBudget": budget }))
        }
        "adaptive" | "auto" => {
            let effort = request
                .get("output_config")
                .and_then(|config| config.get("effort"))
                .and_then(Value::as_str)
                .map(|effort| effort.trim().to_ascii_lowercase())
                .filter(|effort| !effort.is_empty());
            Some(match effort {
                Some(effort) => json!({ "thinkingLevel": effort }),
                None => json!({ "thinkingLevel": "high" }),
            })
        }
        _ => None,
    }
}

/// A trailing model turn whose function calls nothing answers makes Gemini
/// reject the request, so it is dropped (upstream does the same).
fn strip_unanswered_model_turn(contents: &mut Vec<Value>) {
    let Some(last) = contents.last() else {
        return;
    };
    if last.get("role").and_then(Value::as_str) != Some("model") {
        return;
    }
    let has_call = last
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(|part| part.get("functionCall").is_some()));
    if has_call {
        contents.pop();
    }
}

/// Consecutive user turns merge; model turns never do, because merging them
/// would shift part indexes and break reasoning replay.
fn merge_adjacent_user_turns(contents: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for content in contents {
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if parts.is_empty() {
            continue;
        }
        if role == "user" {
            if let Some(last) = merged.last_mut() {
                if last.get("role").and_then(Value::as_str) == Some("user") {
                    let mut combined = last
                        .get("parts")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    combined.extend(parts);
                    last["parts"] = Value::Array(reorder_user_parts(combined));
                    continue;
                }
            }
        }
        merged.push(gemini_content(role, parts));
    }
    merged
}

/// Gemini expects prompt text before `functionResponse` parts inside one user
/// turn (Vertex rejects a turn that ends with text after tool results).
fn reorder_user_parts(parts: Vec<Value>) -> Vec<Value> {
    let has_response = parts
        .iter()
        .any(|part| part.get("functionResponse").is_some());
    if !has_response {
        return parts;
    }
    let mut text: Vec<Value> = Vec::new();
    let mut rest: Vec<Value> = Vec::new();
    for part in parts {
        if part.get("text").is_some() {
            text.push(part);
        } else {
            rest.push(part);
        }
    }
    text.extend(rest);
    text
}

/// Order `tool_result` blocks by the ids the preceding assistant turn called,
/// leaving non-result blocks at their existing slots. Any mismatch leaves the
/// original order alone.
fn align_tool_results(content: &Value, tool_use_ids: &[String]) -> Value {
    let Some(blocks) = content.as_array() else {
        return content.clone();
    };
    let result_indexes: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|(index, _)| index)
        .collect();
    if result_indexes.len() != tool_use_ids.len() {
        return content.clone();
    }
    let mut ordered: Vec<Option<Value>> = result_indexes
        .iter()
        .map(|index| Some(blocks[*index].clone()))
        .collect();
    let mut aligned: Vec<Value> = Vec::with_capacity(tool_use_ids.len());
    for id in tool_use_ids {
        let position = ordered.iter().position(|candidate| {
            candidate
                .as_ref()
                .and_then(|block| block.get("tool_use_id"))
                .and_then(Value::as_str)
                == Some(id)
        });
        match position {
            Some(position) => aligned.push(ordered[position].take().unwrap_or(Value::Null)),
            None => return content.clone(),
        }
    }
    let mut blocks = blocks.clone();
    for (slot, block) in result_indexes.iter().zip(aligned) {
        blocks[*slot] = block;
    }
    Value::Array(blocks)
}

/// Claude tool ids look like `toolu_...` or `<name>-<counter>`; upstream derives
/// the function name from everything before the last dash.
fn tool_name_from_id(id: &str) -> Option<String> {
    let (name, _) = id.rsplit_once('-')?;
    (!name.is_empty() && name != id).then(|| name.to_string())
}

/// Tool names must match `[a-zA-Z0-9_-]{1,64}`; MCP servers routinely hand over
/// names with dots or spaces.
fn sanitize_tool_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    sanitized.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(body: Value) -> Value {
        translate_messages_request("gemini-2.5-pro", body.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn system_prompt_and_text_turns_map_to_contents() {
        let out = translate(json!({
            "model": "claude-sonnet-4",
            "max_tokens": 1024,
            "system": [{"type": "text", "text": "be terse"}],
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": [{"type": "text", "text": "hi"}]},
                {"role": "user", "content": [{"type": "text", "text": "again"}]}
            ]
        }));
        assert_eq!(out["model"], "gemini-2.5-pro");
        assert_eq!(out["systemInstruction"]["role"], "user");
        assert_eq!(out["systemInstruction"]["parts"][0]["text"], "be terse");
        assert_eq!(out["contents"][0]["role"], "user");
        assert_eq!(out["contents"][0]["parts"][0]["text"], "hello");
        assert_eq!(out["contents"][1]["role"], "model");
        assert_eq!(out["contents"][2]["parts"][0]["text"], "again");
        assert_eq!(out["generationConfig"]["maxOutputTokens"], 1024.0);
        assert_eq!(out["safetySettings"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn system_string_is_accepted_and_empty_system_is_omitted() {
        let out = translate(json!({
            "model": "m",
            "system": "policy",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "q"}]}]
        }));
        assert_eq!(out["systemInstruction"]["parts"][0]["text"], "policy");
        let absent = translate(json!({
            "model": "m",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "q"}]}]
        }));
        assert!(absent.get("systemInstruction").is_none());
    }

    #[test]
    fn base64_images_become_inline_data_and_urls_are_refused() {
        let out = translate(json!({
            "model": "m",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAA"}}
            ]}]
        }));
        let parts = out["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[1]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[1]["inline_data"]["data"], "AAA");

        let error = translate_messages_request(
            "gemini-2.5-pro",
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": [
                    {"type": "image", "source": {"type": "url", "url": "https://example.test/a.png"}}
                ]}]
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap_err();
        assert!(
            matches!(error, GeminiIngressError::UnsupportedPart(ref part) if part.contains("url"))
        );
    }

    #[test]
    fn tool_use_and_result_round_trip_with_the_call_name() {
        let out = translate(json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "read a file"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "Read.File", "input": {"path": "a.rs"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        {"type": "text", "text": "contents"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "BBB"}}
                    ]}
                ]}
            ]
        }));
        let contents = out["contents"].as_array().unwrap();
        let call = &contents[1]["parts"][0]["functionCall"];
        assert_eq!(call["name"], "Read_File");
        assert_eq!(call["id"], "toolu_1");
        assert_eq!(call["args"]["path"], "a.rs");
        let response = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(response["name"], "Read_File");
        assert_eq!(response["id"], "toolu_1");
        assert_eq!(response["response"]["result"]["text"], "contents");
        assert_eq!(contents[2]["parts"][1]["inline_data"]["data"], "BBB");
    }

    #[test]
    fn tools_and_choice_map_to_function_declarations() {
        let out = translate(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "q"}],
            "tools": [{
                "name": "Read.File",
                "description": "read",
                "strict": true,
                "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}
            }],
            "tool_choice": {"type": "tool", "name": "Read.File"}
        }));
        let declaration = &out["tools"][0]["functionDeclarations"][0];
        assert_eq!(declaration["name"], "Read_File");
        assert_eq!(declaration["description"], "read");
        assert_eq!(declaration["parametersJsonSchema"]["type"], "object");
        assert!(declaration.get("strict").is_none());
        assert_eq!(out["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            out["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "Read_File"
        );
    }

    #[test]
    fn strict_tools_without_a_choice_ask_for_validation() {
        let out = translate(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "q"}],
            "tools": [{"name": "t", "strict": true, "input_schema": {"type": "object"}}]
        }));
        assert_eq!(
            out["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );
    }

    #[test]
    fn thinking_maps_to_a_budget_or_a_level() {
        let enabled = translate(json!({
            "model": "m",
            "thinking": {"type": "enabled", "budget_tokens": 4096},
            "messages": [{"role": "user", "content": "q"}]
        }));
        assert_eq!(
            enabled["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            4096
        );
        let adaptive = translate(json!({
            "model": "m",
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "LOW"},
            "messages": [{"role": "user", "content": "q"}]
        }));
        assert_eq!(
            adaptive["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "low"
        );
        let default_adaptive = translate(json!({
            "model": "m",
            "thinking": {"type": "adaptive"},
            "messages": [{"role": "user", "content": "q"}]
        }));
        assert_eq!(
            default_adaptive["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
    }

    #[test]
    fn an_unanswered_trailing_model_turn_is_stripped() {
        let out = translate(json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "q"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_9", "name": "t", "input": {}}
                ]}
            ]
        }));
        assert_eq!(out["contents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn adjacent_user_turns_merge_and_blank_text_is_dropped() {
        let out = translate(json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "one"}]},
                {"role": "user", "content": [{"type": "text", "text": "two"}]},
                {"role": "user", "content": [{"type": "text", "text": "   "}]}
            ]
        }));
        let contents = out["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["parts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_mid_conversation_system_message_becomes_a_reminder_turn() {
        let out = translate(json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "q"},
                {"role": "system", "content": [{"type": "text", "text": "stay in scope"}]}
            ]
        }));
        // The reminder turn is a user turn, so it merges into the preceding one.
        let parts = out["contents"][0]["parts"].as_array().unwrap();
        let text = parts[1]["text"].as_str().unwrap();
        assert!(text.starts_with("<system-reminder>"));
        assert!(text.contains("stay in scope"));
    }

    #[test]
    fn malformed_requests_are_refused_by_name() {
        let not_json = translate_messages_request("x", b"nope").unwrap_err();
        assert_eq!(not_json, GeminiIngressError::NotJson);
        let not_object = translate_messages_request("x", b"[]").unwrap_err();
        assert_eq!(not_object, GeminiIngressError::NotJson);
        let no_model =
            translate_messages_request("", br#"{"messages":[{"role":"user","content":"q"}]}"#)
                .unwrap_err();
        assert_eq!(no_model, GeminiIngressError::MissingModel);
        let no_messages =
            translate_messages_request("m", br#"{"model":"x","messages":[]}"#).unwrap_err();
        assert_eq!(no_messages, GeminiIngressError::EmptyMessages);
    }

    #[test]
    fn an_empty_model_argument_keeps_the_requested_model() {
        let out = translate_messages_request(
            "",
            br#"{"model":"claude-sonnet-4","messages":[{"role":"user","content":"q"}]}"#,
        )
        .unwrap();
        assert_eq!(out["model"], "claude-sonnet-4");
    }
}
