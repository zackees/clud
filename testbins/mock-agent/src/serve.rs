//! `mock-agent serve`: a scripted Anthropic Messages backend for driving the
//! real Claude Code harness in tests (#1323).
//!
//! Claude Code runs unmodified with `ANTHROPIC_BASE_URL` pointed here. Every
//! `POST /v1/messages` is answered from a JSON script:
//!
//! - **Role.** The first role whose `match` substring appears in the
//!   request's system prompt answers it; a role without `match` is the
//!   fallback (normally the main session).
//! - **Step.** The step index is the number of assistant turns already in the
//!   request's `messages`. Requests are therefore self-describing: no server
//!   state, and Claude Code's extra user messages (reminders after a
//!   `tool_result`, background-agent completions) never desynchronize it.
//! - **Expect.** A step may assert on the `tool_result`s the harness sent back
//!   for the previous step. A failed expectation is logged and answered with a
//!   `MOCK_EXPECT_FAILED` text turn, which ends the conversation visibly.
//!
//! Every request is appended to `--log` as one JSON line. See the script
//! format in `src/README.md`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

struct Server {
    script: Value,
    log: Mutex<Option<std::fs::File>>,
    counter: AtomicU64,
}

pub fn run(args: &[String]) -> i32 {
    let mut script_path: Option<PathBuf> = None;
    let mut port: u16 = 0;
    let mut log_path: Option<PathBuf> = None;
    let mut port_file: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--script" => script_path = iter.next().map(PathBuf::from),
            "--port" => port = iter.next().and_then(|p| p.parse().ok()).unwrap_or(0),
            "--log" => log_path = iter.next().map(PathBuf::from),
            "--port-file" => port_file = iter.next().map(PathBuf::from),
            other => {
                eprintln!("mock-agent serve: unknown argument {other}");
                return 2;
            }
        }
    }
    let script = match script_path {
        Some(path) => match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        {
            Ok(script) => script,
            Err(error) => {
                eprintln!("mock-agent serve: bad script {}: {error}", path.display());
                return 2;
            }
        },
        None => json!({"roles": []}),
    };
    let log = log_path.map(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open --log")
    });
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("mock-agent serve: bind 127.0.0.1:{port}: {error}");
            return 2;
        }
    };
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    if let Some(path) = port_file {
        let _ = std::fs::write(path, bound.to_string());
    }
    println!("listening {bound}");
    let _ = std::io::stdout().flush();

    let server = Arc::new(Server {
        script,
        log: Mutex::new(log),
        counter: AtomicU64::new(0),
    });
    for stream in listener.incoming().flatten() {
        let server = Arc::clone(&server);
        std::thread::spawn(move || {
            let _ = handle(&server, stream);
        });
    }
    0
}

fn handle(server: &Server, stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
        let route = path.split('?').next().unwrap_or("");
        match (method.as_str(), route) {
            ("POST", p) if p.ends_with("/v1/messages") => {
                let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                respond_messages(server, &mut stream, &request)?;
            }
            ("POST", p) if p.ends_with("/count_tokens") => {
                write_json(&mut stream, 200, &json!({"input_tokens": 1}))?;
            }
            ("GET", p) if p.ends_with("/v1/models") => {
                write_json(&mut stream, 200, &json!({"data": [], "has_more": false}))?;
            }
            _ => write_json(
                &mut stream,
                404,
                &json!({"type": "error", "error": {"type": "not_found_error", "message": route}}),
            )?,
        }
    }
}

fn write_json(stream: &mut TcpStream, status: u16, value: &Value) -> std::io::Result<()> {
    let body = value.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        if status == 200 { "OK" } else { "Error" },
        body.len()
    )?;
    stream.flush()
}

fn system_text(request: &Value) -> String {
    match request.get("system") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Tool results sent since the last assistant turn.
fn recent_tool_results(messages: &[Value]) -> Vec<Value> {
    let mut results = Vec::new();
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) == Some("assistant") {
            break;
        }
        if let Some(Value::Array(blocks)) = message.get("content") {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    let content = match block.get("content") {
                        Some(Value::String(text)) => text.clone(),
                        Some(Value::Array(parts)) => parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        _ => String::new(),
                    };
                    results.push(json!({
                        "is_error": block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        "content": content,
                    }));
                }
            }
        }
    }
    results.reverse();
    results
}

/// Pick the role, step and action for one request.
fn plan(script: &Value, request: &Value) -> (String, usize, Value, Option<String>) {
    let system = system_text(request);
    let tools: Vec<&str> = request
        .get("tools")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let turn = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .count();
    let roles = script
        .get("roles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let matched = roles
        .iter()
        .find(|r| {
            r.get("match")
                .and_then(Value::as_str)
                .is_some_and(|m| system.contains(m))
        })
        .or_else(|| roles.iter().find(|r| r.get("match").is_none()));
    let default_text = script
        .get("default_text")
        .and_then(Value::as_str)
        .unwrap_or("MOCK_DONE");
    let Some(role) = matched else {
        return (
            "unmatched".into(),
            turn,
            json!({"text": default_text}),
            None,
        );
    };
    let name = role
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("role")
        .to_string();
    // A role that needs a tool the request does not offer answers with text,
    // so a background title/summary request never receives a tool_use.
    let steps = role
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(step) = steps.get(turn).cloned() else {
        return (name, turn, json!({"text": default_text}), None);
    };
    if let Some(needed) = step
        .get("tool_use")
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
    {
        if !tools.contains(&needed) && needed != "StructuredOutput" {
            return (
                name,
                turn,
                json!({"text": default_text}),
                Some(format!("tool {needed} not offered")),
            );
        }
    }
    let expect_failure = step.get("expect").and_then(|expect| {
        let results = recent_tool_results(&messages);
        let joined = results
            .iter()
            .filter_map(|r| r.get("content").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(want) = expect.get("is_error").and_then(Value::as_bool) {
            let got = results
                .iter()
                .any(|r| r.get("is_error").and_then(Value::as_bool) == Some(true));
            if got != want {
                return Some(format!("expected is_error={want}, got {got}: {joined}"));
            }
        }
        if let Some(needle) = expect.get("content_contains").and_then(Value::as_str) {
            if !joined.contains(needle) {
                return Some(format!(
                    "expected tool_result to contain {needle:?}, got {joined:?}"
                ));
            }
        }
        None
    });
    if let Some(failure) = expect_failure {
        return (
            name,
            turn,
            json!({"text": format!("MOCK_EXPECT_FAILED: {failure}")}),
            Some(failure),
        );
    }
    (name, turn, step, None)
}

fn respond_messages(
    server: &Server,
    stream: &mut TcpStream,
    request: &Value,
) -> std::io::Result<()> {
    let (role, turn, step, note) = plan(&server.script, request);
    let n = server.counter.fetch_add(1, Ordering::SeqCst) + 1;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("claude");
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Ok(mut guard) = server.log.lock() {
        if let Some(file) = guard.as_mut() {
            let tools: Vec<Value> = request
                .get("tools")
                .and_then(Value::as_array)
                .map(|t| t.iter().filter_map(|t| t.get("name").cloned()).collect())
                .unwrap_or_default();
            let entry = json!({
                "n": n, "role": role, "turn": turn, "step": step, "note": note,
                "stream": request.get("stream"), "tools": tools,
                "tool_results": recent_tool_results(&messages),
                "system": system_text(request),
                "messages": messages,
            });
            let _ = writeln!(file, "{entry}");
        }
    }

    if let Some(error) = step.get("error") {
        let status = error.get("status").and_then(Value::as_u64).unwrap_or(500) as u16;
        return write_json(
            stream,
            status,
            &json!({"type": "error", "error": {"type": "api_error", "message": "mock error"}}),
        );
    }

    let id = format!("msg_mock_{n}");
    let (block, stop) = if let Some(tool) = step.get("tool_use") {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("Bash");
        let input = tool.get("input").cloned().unwrap_or_else(|| json!({}));
        (
            json!({"type": "tool_use", "id": format!("toolu_mock_{n}"), "name": name, "input": input}),
            "tool_use",
        )
    } else if let Some(structured) = step.get("structured") {
        (
            json!({"type": "tool_use", "id": format!("toolu_mock_{n}"), "name": "StructuredOutput", "input": structured}),
            "tool_use",
        )
    } else {
        let text = step
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("MOCK_DONE");
        (json!({"type": "text", "text": text}), "end_turn")
    };

    if request.get("stream").and_then(Value::as_bool) != Some(true) {
        let message = json!({
            "id": id, "type": "message", "role": "assistant", "model": model,
            "content": [block], "stop_reason": stop, "stop_sequence": null,
            "usage": {"input_tokens": 1, "output_tokens": 1},
        });
        return write_json(stream, 200, &message);
    }

    let mut events: Vec<(&str, Value)> = vec![(
        "message_start",
        json!({"type": "message_start", "message": {
            "id": id, "type": "message", "role": "assistant", "model": model, "content": [],
            "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 1, "output_tokens": 1}}}),
    )];
    if block["type"] == "text" {
        events.push((
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ));
        events.push((
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": block["text"]}}),
        ));
    } else {
        events.push((
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}}),
        ));
        events.push((
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {
                "type": "input_json_delta", "partial_json": block["input"].to_string()}}),
        ));
    }
    events.push((
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 0}),
    ));
    events.push((
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 1}}),
    ));
    events.push(("message_stop", json!({"type": "message_stop"})));

    let mut body = String::new();
    for (name, data) in events {
        body.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(system: &str, messages: Value, tools: &[&str]) -> Value {
        json!({
            "system": [{"type": "text", "text": system}],
            "messages": messages,
            "tools": tools.iter().map(|t| json!({"name": t})).collect::<Vec<_>>(),
        })
    }

    fn script() -> Value {
        json!({
            "default_text": "END",
            "roles": [
                {"name": "worker", "match": "You are a /grind worker", "steps": [
                    {"tool_use": {"name": "Bash", "input": {"command": "cargo build"}}},
                    {"expect": {"is_error": true, "content_contains": "BLOCKED"}, "text": "denied as expected"}
                ]},
                {"name": "main", "steps": [
                    {"tool_use": {"name": "Bash", "input": {"command": "echo hi"}}}
                ]}
            ]
        })
    }

    #[test]
    fn role_is_chosen_by_system_prompt_marker_with_a_fallback() {
        let (role, ..) = plan(
            &script(),
            &request("You are a /grind worker.", json!([]), &["Bash"]),
        );
        assert_eq!(role, "worker");
        let (role, ..) = plan(&script(), &request("main session", json!([]), &["Bash"]));
        assert_eq!(role, "main");
    }

    #[test]
    fn step_is_the_number_of_assistant_turns() {
        let first = plan(
            &script(),
            &request(
                "main",
                json!([{"role": "user", "content": "go"}]),
                &["Bash"],
            ),
        );
        assert_eq!(first.2["tool_use"]["name"], "Bash");
        // A reminder message after the tool result does not advance the step.
        let later = json!([
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [{"type": "tool_use"}]},
            {"role": "user", "content": [{"type": "tool_result", "content": "hi"}]},
            {"role": "user", "content": "<system-reminder>"}
        ]);
        let second = plan(&script(), &request("main", later, &["Bash"]));
        assert_eq!(second.1, 1);
        assert_eq!(second.2["text"], "END");
    }

    #[test]
    fn expect_checks_the_previous_tool_results() {
        let denied = json!([
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [{"type": "tool_use"}]},
            {"role": "user", "content": [{"type": "tool_result", "is_error": true, "content": "BLOCKED by caps"}]}
        ]);
        let ok = plan(
            &script(),
            &request("You are a /grind worker", denied, &["Bash"]),
        );
        assert_eq!(ok.2["text"], "denied as expected");
        assert!(ok.3.is_none());

        let allowed = json!([
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [{"type": "tool_use"}]},
            {"role": "user", "content": [{"type": "tool_result", "content": "Compiling"}]}
        ]);
        let failed = plan(
            &script(),
            &request("You are a /grind worker", allowed, &["Bash"]),
        );
        assert!(failed.2["text"]
            .as_str()
            .unwrap()
            .starts_with("MOCK_EXPECT_FAILED"));
    }

    #[test]
    fn a_tool_the_request_does_not_offer_becomes_text() {
        let (_, _, step, note) = plan(&script(), &request("main", json!([]), &[]));
        assert_eq!(step["text"], "END");
        assert!(note.unwrap().contains("not offered"));
    }
}
