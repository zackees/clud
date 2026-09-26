use clud::paste_image::kitty_clipboard_payload;
use clud::paste_image::KittyPastePayload;
use std::io::{self, Write};

fn main() {
    let code = run_with(
        kitty_clipboard_payload(),
        &mut io::stdout(),
        &mut io::stderr(),
    );
    std::process::exit(code);
}

fn run_with(
    read: io::Result<KittyPastePayload>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    match read {
        Ok(payload) => {
            let json = match serde_json::to_string(&payload) {
                Ok(json) => json,
                Err(error) => {
                    let _ = writeln!(stderr, "clipboard JSON failed: {error}");
                    return 1;
                }
            };
            if writeln!(stdout, "{json}").is_err() {
                let _ = writeln!(stderr, "clipboard output failed");
                return 1;
            }
            0
        }
        Err(error) => {
            let _ = writeln!(stderr, "clipboard snapshot failed: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_one_json_object_for_text_without_logs_on_stdout() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with(
            Ok(KittyPastePayload {
                kind: "text",
                value: "hello 🦀".into(),
                bytes: "hello 🦀".len(),
            }),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let payload: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(payload["kind"], "text");
        assert_eq!(payload["value"], "hello 🦀");
        assert_eq!(payload["bytes"], "hello 🦀".len());
        assert_eq!(stdout.iter().filter(|&&byte| byte == b'\n').count(), 1);
    }

    #[test]
    fn emits_image_path_and_file_size_in_json() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with(
            Ok(KittyPastePayload {
                kind: "image",
                value: "C:/Users/test/Pictures/clud-kitty-pastes/paste-1.png".into(),
                bytes: 1234,
            }),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let payload: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(payload["kind"], "image");
        assert_eq!(payload["bytes"], 1234);
        assert_eq!(
            payload["value"],
            "C:/Users/test/Pictures/clud-kitty-pastes/paste-1.png"
        );
    }

    #[test]
    fn failure_returns_nonzero_without_stdout_json() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with(
            Err(io::Error::new(io::ErrorKind::NotFound, "empty clipboard")),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert!(String::from_utf8(stderr)
            .unwrap()
            .contains("empty clipboard"));
    }
}
