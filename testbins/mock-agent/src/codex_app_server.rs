//! Minimal Codex App Server protocol for clud's startup model probe.

use std::io::{self, BufRead, Write};

pub fn run() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id") else {
            continue;
        };
        let response = match request.get("method").and_then(serde_json::Value::as_str) {
            Some("initialize") => serde_json::json!({"id": id, "result": {"capabilities": {}}}),
            Some("model/list") => {
                serde_json::json!({"id": id, "result": {"data": [], "nextCursor": null}})
            }
            _ => serde_json::json!({
                "id": id,
                "error": {"code": -32601, "message": "unsupported mock method"}
            }),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}
