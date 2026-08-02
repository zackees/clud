//! Anthropic Messages request -> OpenAI Responses request translation
//! (issue #627 phase 3, step 2).
//!
//! Deliberately free of HTTP, sockets, and credentials: the bridge owns those,
//! and keeping the mapping a pure function is what lets the table tests below
//! drive every shape directly. Step 5 wires this into
//! [`crate::codex_bridge`]; until then nothing calls it in production.
//!
//! The guiding rule is the issue's last acceptance criterion: **unsupported
//! semantics fail explicitly rather than being silently dropped.** A request
//! that cannot be represented is an error, never a best-effort approximation,
//! because a silently-dropped `tool_choice` or stop sequence surfaces to the
//! user as a model that ignored its instructions.

use serde::{Deserialize, Serialize};

/// The Codex model used when the caller did not name one we should honour.
/// Claude Code always sends its own (Claude) model id, so in practice this is
/// what nearly every cross-route request resolves to.
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5.6-sol";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    /// The request is well-formed but names something the Responses API has no
    /// faithful equivalent for.
    Unsupported(String),
    /// The request does not satisfy the Messages API's own contract.
    Invalid(String),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(formatter, "unsupported by the Codex bridge: {what}"),
            Self::Invalid(what) => write!(formatter, "invalid Messages request: {what}"),
        }
    }
}

impl std::error::Error for TranslateError {}

fn unsupported(what: impl Into<String>) -> TranslateError {
    TranslateError::Unsupported(what.into())
}

fn invalid(what: impl Into<String>) -> TranslateError {
    TranslateError::Invalid(what.into())
}

// ---------------------------------------------------------------------------
// Incoming: Anthropic Messages
// ---------------------------------------------------------------------------

/// Unknown fields are tolerated on purpose: Anthropic adds request fields
/// regularly, and a `deny_unknown_fields` here would turn every additive API
/// change into a hard outage on a route that was otherwise fine. Fields that
/// change *meaning* are rejected explicitly below instead.
#[derive(Debug, Deserialize)]
pub struct MessagesRequest {
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub messages: Vec<Message>,
    pub system: Option<SystemPrompt>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub tool_choice: Option<ToolChoice>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub stream: bool,
    pub thinking: Option<Thinking>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Content,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    /// Captured so the error names the block rather than falling into the
    /// catch-all. Refused rather than dropped: see
    /// `unrepresentable_inputs_are_rejected_not_dropped`.
    Thinking {},
    RedactedThinking {},
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ToolChoice {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    #[serde(default)]
    pub disable_parallel_tool_use: bool,
}

#[derive(Debug, Deserialize)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub kind: String,
    pub budget_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Outgoing: OpenAI Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Vec<InputItem>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponsesTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ResponsesToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
}

/// A Messages `content` array does not map one-to-one onto Responses items: a
/// single assistant turn holding text plus two `tool_use` blocks becomes one
/// message item and two `function_call` items. Order is preserved because the
/// model reads these as a transcript.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Message {
        role: String,
        content: Vec<ContentPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ResponsesTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum ResponsesToolChoice {
    Mode(&'static str),
    Function {
        #[serde(rename = "type")]
        kind: &'static str,
        name: String,
    },
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Reasoning {
    pub effort: &'static str,
}

// ---------------------------------------------------------------------------
// Translation
// ---------------------------------------------------------------------------

/// Resolve the upstream model.
///
/// Claude Code sends its own model id, which would be meaningless upstream, so
/// any `claude*` id resolves to the Codex default. A non-Claude id is honoured
/// verbatim: that is the "explicit model override" the issue asks to preserve,
/// and it is how a caller pins a specific Codex model.
pub fn resolve_model(requested: Option<&str>, default_model: &str) -> String {
    match requested.map(str::trim).filter(|value| !value.is_empty()) {
        Some(model) if !model.to_ascii_lowercase().starts_with("claude") => model.to_string(),
        _ => default_model.to_string(),
    }
}

/// Map Anthropic's token-denominated thinking budget onto the Responses API's
/// coarse effort ladder. The thresholds are a judgement call, not a documented
/// equivalence — they exist so the two ends of the range do not collapse into
/// one effort level.
fn reasoning_effort(thinking: &Thinking) -> Result<Option<Reasoning>, TranslateError> {
    match thinking.kind.as_str() {
        "disabled" => Ok(None),
        "enabled" => {
            let effort = match thinking.budget_tokens.unwrap_or(0) {
                0..=4_095 => "low",
                4_096..=16_383 => "medium",
                _ => "high",
            };
            Ok(Some(Reasoning { effort }))
        }
        other => Err(unsupported(format!("thinking.type {other:?}"))),
    }
}

fn system_text(system: &SystemPrompt) -> Result<String, TranslateError> {
    match system {
        SystemPrompt::Text(text) => Ok(text.clone()),
        SystemPrompt::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if block.kind != "text" {
                    return Err(unsupported(format!("system block type {:?}", block.kind)));
                }
                let text = block
                    .text
                    .as_ref()
                    .ok_or_else(|| invalid("system text block without text"))?;
                parts.push(text.clone());
            }
            Ok(parts.join("\n\n"))
        }
    }
}

fn image_url(source: &ImageSource) -> Result<String, TranslateError> {
    match source {
        // Responses takes one URL field for both cases, so a base64 payload
        // travels as a data URL rather than as a distinct source shape.
        ImageSource::Base64 { media_type, data } => Ok(format!("data:{media_type};base64,{data}")),
        ImageSource::Url { url } => Ok(url.clone()),
        ImageSource::Other => Err(unsupported("image source type")),
    }
}

/// Flatten a tool result into the single string `function_call_output` carries.
/// An image inside a tool result has nowhere to go and is refused rather than
/// dropped, which would otherwise look to the model like the tool returned
/// nothing.
fn tool_result_output(
    content: Option<&ToolResultContent>,
    is_error: bool,
) -> Result<String, TranslateError> {
    let body = match content {
        None => String::new(),
        Some(ToolResultContent::Text(text)) => text.clone(),
        Some(ToolResultContent::Blocks(blocks)) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => parts.push(text.clone()),
                    ContentBlock::Image { .. } => {
                        return Err(unsupported("image block inside tool_result"));
                    }
                    _ => return Err(unsupported("non-text block inside tool_result")),
                }
            }
            parts.join("\n")
        }
    };
    Ok(if is_error {
        format!("ERROR: {body}")
    } else {
        body
    })
}

fn push_message(items: &mut Vec<InputItem>, role: &str, parts: Vec<ContentPart>) {
    if !parts.is_empty() {
        items.push(InputItem::Message {
            role: role.to_string(),
            content: parts,
        });
    }
}

fn translate_message(message: &Message, items: &mut Vec<InputItem>) -> Result<(), TranslateError> {
    let role = match message.role.as_str() {
        role @ ("user" | "assistant") => role,
        other => return Err(invalid(format!("message role {other:?}"))),
    };
    // Assistant turns are replayed as model output, user turns as input; the
    // Responses API distinguishes the two by content part type.
    let text_part = |text: String| {
        if role == "assistant" {
            ContentPart::OutputText { text }
        } else {
            ContentPart::InputText { text }
        }
    };

    match &message.content {
        Content::Text(text) => {
            push_message(items, role, vec![text_part(text.clone())]);
        }
        Content::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => parts.push(text_part(text.clone())),
                    ContentBlock::Image { source } => {
                        if role == "assistant" {
                            return Err(unsupported("image block in an assistant message"));
                        }
                        parts.push(ContentPart::InputImage {
                            image_url: image_url(source)?,
                        });
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        // Flush the text accumulated before this call so the
                        // transcript keeps its original order.
                        push_message(items, role, std::mem::take(&mut parts));
                        items.push(InputItem::FunctionCall {
                            call_id: id.clone(),
                            name: name.clone(),
                            arguments: serde_json::to_string(input)
                                .map_err(|error| invalid(format!("tool_use input: {error}")))?,
                        });
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        push_message(items, role, std::mem::take(&mut parts));
                        items.push(InputItem::FunctionCallOutput {
                            call_id: tool_use_id.clone(),
                            output: tool_result_output(content.as_ref(), *is_error)?,
                        });
                    }
                    ContentBlock::Thinking {} | ContentBlock::RedactedThinking {} => {
                        return Err(unsupported("thinking block in request history"));
                    }
                    ContentBlock::Other => return Err(unsupported("content block type")),
                }
            }
            push_message(items, role, parts);
        }
    }
    Ok(())
}

fn translate_tool_choice(
    choice: &ToolChoice,
) -> Result<(Option<ResponsesToolChoice>, Option<bool>), TranslateError> {
    let mapped = match choice.kind.as_str() {
        "auto" => ResponsesToolChoice::Mode("auto"),
        "any" => ResponsesToolChoice::Mode("required"),
        "none" => ResponsesToolChoice::Mode("none"),
        "tool" => {
            let name = choice
                .name
                .clone()
                .ok_or_else(|| invalid("tool_choice type \"tool\" without a name"))?;
            ResponsesToolChoice::Function {
                kind: "function",
                name,
            }
        }
        other => return Err(unsupported(format!("tool_choice type {other:?}"))),
    };
    // Anthropic expresses this as an opt-out; Responses as an opt-in. Only send
    // the field when the caller actually asked to disable parallel calls.
    let parallel = choice.disable_parallel_tool_use.then_some(false);
    Ok((Some(mapped), parallel))
}

/// Translate a decoded Messages request into a Responses request.
pub fn translate_request(
    request: &MessagesRequest,
    default_model: &str,
) -> Result<ResponsesRequest, TranslateError> {
    if request.messages.is_empty() {
        return Err(invalid("messages must not be empty"));
    }
    // Rejected rather than dropped: both change what the model produces, and
    // the Responses API has no equivalent knob to carry them.
    if request.top_k.is_some() {
        return Err(unsupported("top_k"));
    }
    if request
        .stop_sequences
        .as_ref()
        .is_some_and(|sequences| !sequences.is_empty())
    {
        return Err(unsupported("stop_sequences"));
    }

    let mut input = Vec::new();
    for message in &request.messages {
        translate_message(message, &mut input)?;
    }

    let (tool_choice, parallel_tool_calls) = match &request.tool_choice {
        Some(choice) => translate_tool_choice(choice)?,
        None => (None, None),
    };

    let tools = match &request.tools {
        None => None,
        Some(definitions) => {
            let mut mapped = Vec::with_capacity(definitions.len());
            for definition in definitions {
                mapped.push(ResponsesTool {
                    kind: "function",
                    name: definition.name.clone(),
                    description: definition.description.clone(),
                    parameters: definition
                        .input_schema
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                });
            }
            Some(mapped)
        }
    };

    let reasoning = match &request.thinking {
        Some(thinking) => reasoning_effort(thinking)?,
        None => None,
    };

    Ok(ResponsesRequest {
        model: resolve_model(request.model.as_deref(), default_model),
        input,
        stream: request.stream,
        instructions: match &request.system {
            Some(system) => Some(system_text(system)?),
            None => None,
        },
        tools,
        tool_choice,
        parallel_tool_calls,
        max_output_tokens: request.max_tokens,
        temperature: request.temperature,
        top_p: request.top_p,
        reasoning,
    })
}

/// Decode raw request bytes and translate them in one step.
pub fn translate_bytes(
    body: &[u8],
    default_model: &str,
) -> Result<ResponsesRequest, TranslateError> {
    let request: MessagesRequest = serde_json::from_slice(body)
        .map_err(|error| invalid(format!("could not decode request: {error}")))?;
    translate_request(&request, default_model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn translate(value: serde_json::Value) -> Result<ResponsesRequest, TranslateError> {
        translate_bytes(value.to_string().as_bytes(), DEFAULT_CODEX_MODEL)
    }

    fn ok(value: serde_json::Value) -> ResponsesRequest {
        translate(value).expect("translation should succeed")
    }

    #[test]
    fn text_only_request_maps_roles_and_defaults_the_model() {
        let out = ok(json!({
            "model": "claude-sonnet-5",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"},
                {"role": "user", "content": [{"type": "text", "text": "again"}]}
            ]
        }));

        assert_eq!(out.model, DEFAULT_CODEX_MODEL);
        assert_eq!(out.max_output_tokens, Some(1024));
        assert!(!out.stream);
        assert_eq!(
            out.input,
            vec![
                InputItem::Message {
                    role: "user".into(),
                    content: vec![ContentPart::InputText {
                        text: "hello".into()
                    }],
                },
                InputItem::Message {
                    role: "assistant".into(),
                    // Assistant turns replay as model output, not input.
                    content: vec![ContentPart::OutputText { text: "hi".into() }],
                },
                InputItem::Message {
                    role: "user".into(),
                    content: vec![ContentPart::InputText {
                        text: "again".into()
                    }],
                },
            ]
        );
    }

    #[test]
    fn explicit_non_claude_model_is_preserved() {
        assert_eq!(
            resolve_model(Some("gpt-5.6-codex"), "fallback"),
            "gpt-5.6-codex"
        );
        assert_eq!(
            resolve_model(Some("claude-opus-4-8"), "fallback"),
            "fallback"
        );
        assert_eq!(resolve_model(Some("  "), "fallback"), "fallback");
        assert_eq!(resolve_model(None, "fallback"), "fallback");
        // Case-insensitive: the guard is about the vendor, not the spelling.
        assert_eq!(resolve_model(Some("Claude-3"), "fallback"), "fallback");
    }

    #[test]
    fn system_accepts_both_a_string_and_ordered_blocks() {
        let plain = ok(json!({
            "system": "be brief",
            "messages": [{"role": "user", "content": "x"}]
        }));
        assert_eq!(plain.instructions.as_deref(), Some("be brief"));

        let blocks = ok(json!({
            "system": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ],
            "messages": [{"role": "user", "content": "x"}]
        }));
        assert_eq!(blocks.instructions.as_deref(), Some("first\n\nsecond"));
    }

    #[test]
    fn multi_turn_tool_loop_preserves_transcript_order() {
        let out = ok(json!({
            "messages": [
                {"role": "user", "content": "what is the weather"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "call_1", "name": "weather",
                     "input": {"city": "Paris"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "18C"}
                ]},
                {"role": "assistant", "content": "It is 18C."}
            ]
        }));

        assert_eq!(
            out.input,
            vec![
                InputItem::Message {
                    role: "user".into(),
                    content: vec![ContentPart::InputText {
                        text: "what is the weather".into()
                    }],
                },
                // Text before the call is flushed first so ordering survives.
                InputItem::Message {
                    role: "assistant".into(),
                    content: vec![ContentPart::OutputText {
                        text: "checking".into()
                    }],
                },
                InputItem::FunctionCall {
                    call_id: "call_1".into(),
                    name: "weather".into(),
                    arguments: r#"{"city":"Paris"}"#.into(),
                },
                InputItem::FunctionCallOutput {
                    call_id: "call_1".into(),
                    output: "18C".into(),
                },
                InputItem::Message {
                    role: "assistant".into(),
                    content: vec![ContentPart::OutputText {
                        text: "It is 18C.".into()
                    }],
                },
            ]
        );
    }

    #[test]
    fn parallel_tool_calls_become_separate_function_call_items() {
        let out = ok(json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "a", "name": "one", "input": {}},
                    {"type": "tool_use", "id": "b", "name": "two", "input": {"k": 1}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "a", "content": "ra"},
                    {"type": "tool_result", "tool_use_id": "b", "content": "rb"}
                ]}
            ]
        }));
        assert_eq!(out.input.len(), 4);
        assert!(matches!(
            &out.input[0],
            InputItem::FunctionCall { call_id, name, .. } if call_id == "a" && name == "one"
        ));
        assert!(matches!(
            &out.input[3],
            InputItem::FunctionCallOutput { call_id, output } if call_id == "b" && output == "rb"
        ));
    }

    #[test]
    fn tool_result_blocks_and_errors_flatten_to_one_output_string() {
        let out = ok(json!({
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t", "content": [
                    {"type": "text", "text": "line one"},
                    {"type": "text", "text": "line two"}
                ]},
                {"type": "tool_result", "tool_use_id": "e", "content": "boom",
                 "is_error": true},
                {"type": "tool_result", "tool_use_id": "empty"}
            ]}]
        }));
        assert_eq!(
            out.input,
            vec![
                InputItem::FunctionCallOutput {
                    call_id: "t".into(),
                    output: "line one\nline two".into(),
                },
                InputItem::FunctionCallOutput {
                    call_id: "e".into(),
                    output: "ERROR: boom".into(),
                },
                InputItem::FunctionCallOutput {
                    call_id: "empty".into(),
                    output: String::new(),
                },
            ]
        );
    }

    #[test]
    fn images_travel_as_data_urls_or_plain_urls() {
        let out = ok(json!({
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "QUJD"}},
                {"type": "image", "source": {
                    "type": "url", "url": "https://example.test/a.png"}}
            ]}]
        }));
        assert_eq!(
            out.input,
            vec![InputItem::Message {
                role: "user".into(),
                content: vec![
                    ContentPart::InputImage {
                        image_url: "data:image/png;base64,QUJD".into()
                    },
                    ContentPart::InputImage {
                        image_url: "https://example.test/a.png".into()
                    },
                ],
            }]
        );
    }

    #[test]
    fn tools_and_tool_choice_map_across_every_mode() {
        let base = |choice: serde_json::Value| {
            json!({
                "messages": [{"role": "user", "content": "x"}],
                "tools": [{
                    "name": "lookup",
                    "description": "look things up",
                    "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}}
                }],
                "tool_choice": choice
            })
        };

        let auto = ok(base(json!({"type": "auto"})));
        let tools = auto.tools.as_ref().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, "function");
        assert_eq!(tools[0].name, "lookup");
        assert_eq!(tools[0].description.as_deref(), Some("look things up"));
        assert_eq!(tools[0].parameters["properties"]["q"]["type"], "string");
        assert_eq!(auto.tool_choice, Some(ResponsesToolChoice::Mode("auto")));
        assert_eq!(auto.parallel_tool_calls, None);

        // "any" means "you must call something" -> Responses "required".
        assert_eq!(
            ok(base(json!({"type": "any"}))).tool_choice,
            Some(ResponsesToolChoice::Mode("required"))
        );
        assert_eq!(
            ok(base(json!({"type": "none"}))).tool_choice,
            Some(ResponsesToolChoice::Mode("none"))
        );
        assert_eq!(
            ok(base(json!({"type": "tool", "name": "lookup"}))).tool_choice,
            Some(ResponsesToolChoice::Function {
                kind: "function",
                name: "lookup".into()
            })
        );

        // Opt-out on one side, opt-in on the other.
        let serial = ok(base(
            json!({"type": "auto", "disable_parallel_tool_use": true}),
        ));
        assert_eq!(serial.parallel_tool_calls, Some(false));

        // A tool without a schema still needs valid `parameters` upstream.
        let bare = ok(json!({
            "messages": [{"role": "user", "content": "x"}],
            "tools": [{"name": "noargs"}]
        }));
        assert_eq!(
            bare.tools.expect("tools")[0].parameters,
            json!({"type": "object"})
        );
    }

    #[test]
    fn thinking_budget_maps_onto_the_effort_ladder() {
        let effort = |thinking: serde_json::Value| {
            ok(json!({
                "messages": [{"role": "user", "content": "x"}],
                "thinking": thinking
            }))
            .reasoning
        };

        assert_eq!(
            effort(json!({"type": "enabled", "budget_tokens": 1024})),
            Some(Reasoning { effort: "low" })
        );
        assert_eq!(
            effort(json!({"type": "enabled", "budget_tokens": 8192})),
            Some(Reasoning { effort: "medium" })
        );
        assert_eq!(
            effort(json!({"type": "enabled", "budget_tokens": 32768})),
            Some(Reasoning { effort: "high" })
        );
        assert_eq!(effort(json!({"type": "disabled"})), None);
        assert_eq!(
            ok(json!({"messages": [{"role": "user", "content": "x"}]})).reasoning,
            None
        );
    }

    #[test]
    fn sampling_and_stream_flags_pass_through() {
        let out = ok(json!({
            "messages": [{"role": "user", "content": "x"}],
            "temperature": 0.25,
            "top_p": 0.9,
            "stream": true
        }));
        assert_eq!(out.temperature, Some(0.25));
        assert_eq!(out.top_p, Some(0.9));
        assert!(out.stream);
    }

    /// Every branch that must fail loudly. The acceptance criterion is that
    /// unsupported semantics are refused, so this table is the criterion.
    #[test]
    fn unrepresentable_inputs_are_rejected_not_dropped() {
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "top_k",
                json!({"messages": [{"role": "user", "content": "x"}], "top_k": 5}),
            ),
            (
                "stop_sequences",
                json!({"messages": [{"role": "user", "content": "x"}],
                       "stop_sequences": ["STOP"]}),
            ),
            (
                "thinking block in request history",
                json!({"messages": [{"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "...", "signature": "s"}]}]}),
            ),
            (
                "image block inside tool_result",
                json!({"messages": [{"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t", "content": [
                        {"type": "image", "source": {"type": "url", "url": "u"}}]}]}]}),
            ),
            (
                "image block in an assistant message",
                json!({"messages": [{"role": "assistant", "content": [
                    {"type": "image", "source": {"type": "url", "url": "u"}}]}]}),
            ),
            (
                "image source type",
                json!({"messages": [{"role": "user", "content": [
                    {"type": "image", "source": {"type": "file", "file_id": "f"}}]}]}),
            ),
            (
                "content block type",
                json!({"messages": [{"role": "user", "content": [
                    {"type": "document", "source": {}}]}]}),
            ),
            (
                "tool_choice type",
                json!({"messages": [{"role": "user", "content": "x"}],
                       "tool_choice": {"type": "sometimes"}}),
            ),
            (
                "thinking.type",
                json!({"messages": [{"role": "user", "content": "x"}],
                       "thinking": {"type": "maybe"}}),
            ),
        ];

        for (needle, value) in cases {
            match translate(value) {
                Err(TranslateError::Unsupported(what)) => assert!(
                    what.contains(needle),
                    "expected {needle:?} in unsupported message, got {what:?}"
                ),
                other => panic!("expected {needle:?} to be unsupported, got {other:?}"),
            }
        }
    }

    #[test]
    fn malformed_requests_are_invalid_rather_than_unsupported() {
        assert!(matches!(
            translate(json!({"messages": []})),
            Err(TranslateError::Invalid(_))
        ));
        assert!(matches!(
            translate(json!({"messages": [{"role": "system", "content": "x"}]})),
            Err(TranslateError::Invalid(_))
        ));
        assert!(matches!(
            translate(json!({"messages": [{"role": "user", "content": "x"}],
                             "tool_choice": {"type": "tool"}})),
            Err(TranslateError::Invalid(_))
        ));
        assert!(matches!(
            translate_bytes(b"not json", DEFAULT_CODEX_MODEL),
            Err(TranslateError::Invalid(_))
        ));
    }

    /// Additive Anthropic request fields must not break an otherwise fine
    /// route; fields that change meaning are rejected explicitly instead.
    #[test]
    fn unknown_top_level_fields_are_tolerated() {
        let out = ok(json!({
            "messages": [{"role": "user", "content": "x"}],
            "some_future_field": {"nested": true}
        }));
        assert_eq!(out.model, DEFAULT_CODEX_MODEL);
    }

    /// The wire shape is the contract; assert it rather than only the structs.
    #[test]
    fn serialized_request_omits_absent_fields_and_names_them_correctly() {
        let out = ok(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 64
        }));
        let wire = serde_json::to_value(&out).unwrap();
        assert_eq!(
            wire,
            json!({
                "model": DEFAULT_CODEX_MODEL,
                "stream": false,
                "max_output_tokens": 64,
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hi"}]
                }]
            })
        );
    }
}
