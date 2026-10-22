//! Restore Anthropic SSE frames while keeping a bounded suffix per block.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Cursor, Read};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::reversible::Session;

pub type SharedSession = Arc<Mutex<Session>>;
const FRAME_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Json,
}

impl Kind {
    fn delta_type(self) -> &'static str {
        match self {
            Self::Text => "text_delta",
            Self::Json => "input_json_delta",
        }
    }

    fn field(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "partial_json",
        }
    }
}

struct Pending {
    kind: Kind,
    text: String,
}

pub struct Frames {
    session: SharedSession,
    pending: BTreeMap<u64, Pending>,
}

impl Frames {
    pub fn new(session: SharedSession) -> Self {
        Self {
            session,
            pending: BTreeMap::new(),
        }
    }

    pub fn rewrite(&mut self, frame: &str) -> io::Result<String> {
        let payload = frame
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|data| data.strip_prefix(' ').unwrap_or(data))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(mut document) = serde_json::from_str::<Value>(&payload) else {
            return Ok(frame.to_string());
        };
        let shared = Arc::clone(&self.session);
        let mut session = shared
            .lock()
            .map_err(|_| io::Error::other("secret redaction state unavailable"))?;
        let mut before = String::new();
        let changed = match document.get("type").and_then(Value::as_str) {
            Some("content_block_delta") => self.delta(&mut document, &mut session)?,
            Some("content_block_start") => self.start(&mut document, &mut session)?,
            Some("content_block_stop") => {
                if let Some(index) = document.get("index").and_then(Value::as_u64) {
                    before = self.flush(&mut session, Some(index));
                }
                false
            }
            Some("message_stop") => {
                before = self.flush(&mut session, None);
                false
            }
            _ => false,
        };
        before.push_str(&if changed {
            replace_data(frame, &document)?
        } else {
            frame.to_string()
        });
        Ok(before)
    }

    fn delta(&mut self, document: &mut Value, session: &mut Session) -> io::Result<bool> {
        let Some(index) = document.get("index").and_then(Value::as_u64) else {
            return Ok(false);
        };
        let Some(delta) = document.get_mut("delta") else {
            return Ok(false);
        };
        let kind = match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => Kind::Text,
            Some("input_json_delta") => Kind::Json,
            _ => return Ok(false),
        };
        let Some(text) = delta.get_mut(kind.field()) else {
            return Ok(false);
        };
        let Some(original) = text.as_str() else {
            return Ok(false);
        };
        let pending = self.pending.entry(index).or_insert_with(|| Pending {
            kind,
            text: String::new(),
        });
        if pending.kind != kind {
            return Err(io::Error::other(
                "secret redaction delta type changed within a block",
            ));
        }
        let restored =
            session.restore_fragment(&mut pending.text, original, kind == Kind::Json, false);
        let changed = restored != original;
        *text = Value::String(restored);
        Ok(changed)
    }

    fn start(&mut self, document: &mut Value, session: &mut Session) -> io::Result<bool> {
        let Some(index) = document.get("index").and_then(Value::as_u64) else {
            return Ok(false);
        };
        let Some(block) = document.get_mut("content_block") else {
            return Ok(false);
        };
        if block.get("type").and_then(Value::as_str) == Some("text") {
            let Some(text) = block.get_mut("text") else {
                return Ok(false);
            };
            let Some(original) = text.as_str() else {
                return Ok(false);
            };
            let pending = self.pending.entry(index).or_insert_with(|| Pending {
                kind: Kind::Text,
                text: String::new(),
            });
            let restored = session.restore_fragment(&mut pending.text, original, false, false);
            let changed = restored != original;
            *text = Value::String(restored);
            return Ok(changed);
        }
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            let bytes = serde_json::to_vec(block).map_err(io::Error::other)?;
            let restored = session.restore_body(&bytes).map_err(io::Error::other)?;
            let changed = restored.as_ref() != bytes;
            if changed {
                *block = serde_json::from_slice(&restored).map_err(io::Error::other)?;
            }
            return Ok(changed);
        }
        Ok(false)
    }

    fn flush(&mut self, session: &mut Session, index: Option<u64>) -> String {
        let indices: Vec<u64> = self
            .pending
            .keys()
            .copied()
            .filter(|key| index.is_none_or(|index| *key == index))
            .collect();
        let mut frames = String::new();
        for index in indices {
            let mut pending = self
                .pending
                .remove(&index)
                .expect("collected pending index");
            let text =
                session.restore_fragment(&mut pending.text, "", pending.kind == Kind::Json, true);
            if !text.is_empty() {
                let mut delta = json!({"type": pending.kind.delta_type()});
                delta[pending.kind.field()] = Value::String(text);
                let document = json!({"type":"content_block_delta", "index":index, "delta":delta});
                frames.push_str(&format!("event: content_block_delta\ndata: {document}\n\n"));
            }
        }
        frames
    }

    pub fn finish(&mut self) -> io::Result<String> {
        let shared = Arc::clone(&self.session);
        let mut session = shared
            .lock()
            .map_err(|_| io::Error::other("secret redaction state unavailable"))?;
        Ok(self.flush(&mut session, None))
    }
}

fn replace_data(frame: &str, document: &Value) -> io::Result<String> {
    let encoded = serde_json::to_string(document).map_err(io::Error::other)?;
    let mut output = String::new();
    let mut written = false;
    for line in frame.lines() {
        if line.starts_with("data:") {
            if !written {
                output.push_str("data: ");
                output.push_str(&encoded);
                output.push('\n');
                written = true;
            }
        } else if !line.is_empty() {
            output.push_str(line);
            output.push('\n');
        }
    }
    output.push('\n');
    Ok(output)
}

/// A network reader may split anywhere, including within UTF-8 and SSE JSON.
/// Frame parsing is bounded; only token-prefix tails survive between frames.
pub struct RestoringReader<R> {
    input: BufReader<R>,
    frames: Frames,
    output: Cursor<Vec<u8>>,
    done: bool,
}

impl<R: Read> RestoringReader<R> {
    pub fn new(input: R, session: SharedSession) -> Self {
        Self {
            input: BufReader::new(input),
            frames: Frames::new(session),
            output: Cursor::new(Vec::new()),
            done: false,
        }
    }

    fn next_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut frame = Vec::new();
        loop {
            let start = frame.len();
            let count = (&mut self.input)
                .take((FRAME_LIMIT - start + 1) as u64)
                .read_until(b'\n', &mut frame)?;
            if frame.len() > FRAME_LIMIT {
                return Err(io::Error::other("secret redaction SSE frame exceeds limit"));
            }
            if count == 0 {
                self.done = true;
                let frame = std::str::from_utf8(&frame)
                    .map_err(|_| io::Error::other("invalid UTF-8 SSE frame"))?;
                let mut restored = self.frames.rewrite(frame)?;
                restored.push_str(&self.frames.finish()?);
                return Ok(restored.into_bytes());
            }
            if matches!(&frame[start..], b"\n" | b"\r\n") {
                let frame = std::str::from_utf8(&frame)
                    .map_err(|_| io::Error::other("invalid UTF-8 SSE frame"))?;
                return self.frames.rewrite(frame).map(String::into_bytes);
            }
        }
    }
}

impl<R: Read> Read for RestoringReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            let count = self.output.read(buffer)?;
            if count != 0 || self.done {
                return Ok(count);
            }
            self.output = Cursor::new(self.next_frame()?);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(secret: &str) -> (SharedSession, String) {
        let mut session = Session::with_key([2; 32], vec![secret.to_string()]);
        let token = session.redact_text(secret).into_owned();
        assert!(token.starts_with("clud_secret_"));
        (Arc::new(Mutex::new(session)), token)
    }

    fn frame(document: Value) -> String {
        format!(
            "event: {}\ndata: {document}\n\n",
            document["type"].as_str().unwrap()
        )
    }

    fn delta(index: u64, text: &str, json: bool) -> String {
        let kind = if json { Kind::Json } else { Kind::Text };
        let mut delta = json!({"type":kind.delta_type()});
        delta[kind.field()] = Value::String(text.to_string());
        frame(json!({"type":"content_block_delta","index":index,"delta":delta}))
    }

    fn documents(frames: &str) -> Vec<Value> {
        frames
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn joined(frames: &str, field: &str, index: u64) -> String {
        documents(frames)
            .iter()
            .filter(|document| document["index"] == index)
            .filter_map(|document| {
                document
                    .get("delta")
                    .and_then(|delta| delta.get(field))
                    .and_then(Value::as_str)
            })
            .collect()
    }

    #[test]
    fn sse_text_restores_at_every_token_boundary() {
        let secret = "FAKE_SSE_PRIVATE_CREDENTIAL";
        let (shared, token) = fixture(secret);
        for split in 0..=token.len() {
            let mut frames = Frames::new(Arc::clone(&shared));
            let first = frames.rewrite(&delta(0, &token[..split], false)).unwrap();
            let second = frames.rewrite(&delta(0, &token[split..], false)).unwrap();
            let stop = frames
                .rewrite(&frame(json!({"type":"content_block_stop","index":0})))
                .unwrap();
            let rendered = first + &second + &stop;
            assert_eq!(joined(&rendered, "text", 0), secret, "split {split}");
            assert!(!rendered.contains(&token));
        }
    }

    #[test]
    fn sse_tool_json_restores_escaped_values_at_every_fragment_boundary() {
        let secret = "FAKE_\"quote\\slash\nnewline_\u{1f600}";
        let (shared, token) = fixture(secret);
        let input = format!("{{\"private\":\"{token}\"}}");
        for split in 0..=input.len() {
            let mut frames = Frames::new(Arc::clone(&shared));
            let mut rendered = frames.rewrite(&delta(1, &input[..split], true)).unwrap();
            rendered.push_str(&frames.rewrite(&delta(1, &input[split..], true)).unwrap());
            rendered.push_str(
                &frames
                    .rewrite(&frame(json!({"type":"content_block_stop","index":1})))
                    .unwrap(),
            );
            let json = joined(&rendered, "partial_json", 1);
            assert_eq!(
                serde_json::from_str::<Value>(&json).unwrap()["private"],
                secret,
                "split {split}"
            );
        }
    }

    #[test]
    fn message_stop_flushes_each_blocks_partial_prefix_in_order() {
        let (shared, _) = fixture("FAKE_PRIVATE_CREDENTIAL");
        let mut frames = Frames::new(shared);
        let mut rendered = frames.rewrite(&delta(0, "hello clud_sec", false)).unwrap();
        rendered.push_str(
            &frames
                .rewrite(&delta(1, "other clud_secret_a", false))
                .unwrap(),
        );
        rendered.push_str(
            &frames
                .rewrite(&frame(json!({"type":"message_stop"})))
                .unwrap(),
        );
        assert_eq!(joined(&rendered, "text", 0), "hello clud_sec");
        assert_eq!(joined(&rendered, "text", 1), "other clud_secret_a");
        assert_eq!(documents(&rendered).last().unwrap()["type"], "message_stop");
        assert!(frames.pending.is_empty());
    }

    #[test]
    fn content_block_start_restores_initial_text_and_tool_inputs() {
        let secret = "FAKE_\"INITIAL_KEY\nline";
        let (shared, token) = fixture(secret);
        let mut frames = Frames::new(shared);
        let text = frames
            .rewrite(&frame(json!({"type":"content_block_start","index":0,
            "content_block":{"type":"text","text":token}})))
            .unwrap();
        assert_eq!(documents(&text)[0]["content_block"]["text"], secret);
        let tool = frames
            .rewrite(&frame(json!({"type":"content_block_start","index":1,
            "content_block":{"type":"tool_use","input":{"private":token}}})))
            .unwrap();
        assert_eq!(
            documents(&tool)[0]["content_block"]["input"]["private"],
            secret
        );
    }

    #[test]
    fn untouched_frames_preserve_comments_ids_whitespace_and_crlf() {
        let (shared, _) = fixture("FAKE_PRIVATE_CREDENTIAL");
        let mut frames = Frames::new(shared);
        for frame in [
            ": keepalive\r\n\r\n",
            "id: 42\r\nevent: ping\r\ndata: { \"type\" : \"ping\" }\r\n\r\n",
            "event: ignored\ndata: not-json\n\n",
            "event: content_block_delta\ndata: { \"type\":\"content_block_delta\", \"index\":0, \"delta\":{\"type\":\"text_delta\",\"text\":\"ordinary code.\"}}\n\n",
        ] {
            assert_eq!(frames.rewrite(frame).unwrap(), frame);
        }
    }

    struct ByteReader(Cursor<Vec<u8>>);

    impl Read for ByteReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if buffer.is_empty() {
                return Ok(0);
            }
            self.0.read(&mut buffer[..1])
        }
    }

    #[test]
    fn network_byte_boundaries_preserve_utf8_and_flush_at_eof() {
        let secret = "FAKE_\u{1f600}_KEY\nnext";
        let (shared, token) = fixture(secret);
        let source = delta(0, &format!("\u{1f600} {token} clud_sec"), false);
        let mut reader = RestoringReader::new(ByteReader(Cursor::new(source.into_bytes())), shared);
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        assert_eq!(
            joined(&output, "text", 0),
            format!("\u{1f600} {secret} clud_sec")
        );
    }

    #[test]
    fn rewritten_frames_keep_resume_ids_and_retry_fields() {
        let (shared, token) = fixture("FAKE_PRIVATE_CREDENTIAL");
        let original = format!("id: 42\nretry: 500\n{}", delta(0, &token, false));
        let rendered = Frames::new(shared).rewrite(&original).unwrap();
        assert!(rendered.starts_with("id: 42\nretry: 500\n"));
        assert_eq!(joined(&rendered, "text", 0), "FAKE_PRIVATE_CREDENTIAL");
    }
}
