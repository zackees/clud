//! Normalize native patch envelopes and establish every source/destination (#2289).

use serde_json::Value;

const INPUT_ERROR: &str = "unsupported or ambiguous native patch input: expected a string or matching string patch/input/command/script fields";
const PATCH_ERROR: &str =
    "cannot establish target paths: malformed patch headers, sections, or move syntax";

pub(super) fn targets(input: Option<&Value>) -> Result<Vec<&str>, &'static str> {
    parse_targets(patch_text(input)?)
}

fn patch_text(input: Option<&Value>) -> Result<&str, &'static str> {
    let input = input.ok_or(INPUT_ERROR)?;
    if let Some(text) = input.as_str() {
        return Ok(text);
    }
    let object = input.as_object().ok_or(INPUT_ERROR)?;
    let mut text = None;
    // Codex's freeform bridge delivers patch source in command/script, while
    // other integrations use patch/input. These are data, never shell source.
    for key in ["patch", "input", "command", "script"] {
        if let Some(value) = object.get(key) {
            let value = value.as_str().ok_or(INPUT_ERROR)?;
            if text.is_some_and(|text| text != value) {
                return Err(INPUT_ERROR);
            }
            text = Some(value);
        }
    }
    text.ok_or(INPUT_ERROR)
}

#[derive(Clone, Copy)]
enum Section {
    Add,
    Update,
    Delete,
}

fn target_path(path: &str) -> Result<&str, &'static str> {
    let path = path.trim();
    if path.is_empty() || path.contains('\0') {
        Err(PATCH_ERROR)
    } else {
        Ok(path)
    }
}

fn parse_targets(patch: &str) -> Result<Vec<&str>, &'static str> {
    let mut lines = patch.trim().lines();
    if lines.next() != Some("*** Begin Patch") {
        return Err(PATCH_ERROR);
    }
    let mut targets = Vec::new();
    let mut section = None;
    let mut can_move = false;
    let mut ended = false;
    for line in lines {
        if ended {
            return Err(PATCH_ERROR);
        }
        if line == "*** End Patch" {
            ended = true;
        } else if let Some(path) = line.strip_prefix("*** Add File: ") {
            targets.push(target_path(path)?);
            section = Some(Section::Add);
            can_move = false;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            targets.push(target_path(path)?);
            section = Some(Section::Update);
            can_move = true;
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            targets.push(target_path(path)?);
            section = Some(Section::Delete);
            can_move = false;
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            if !can_move {
                return Err(PATCH_ERROR);
            }
            targets.push(target_path(path)?);
            can_move = false;
        } else {
            can_move = false;
            if !valid_body_line(section, line) {
                return Err(PATCH_ERROR);
            }
        }
    }
    if !ended || targets.is_empty() {
        return Err(PATCH_ERROR);
    }
    Ok(targets)
}

fn valid_body_line(section: Option<Section>, line: &str) -> bool {
    match section {
        Some(Section::Add) => line.starts_with('+'),
        Some(Section::Update) => {
            line.is_empty()
                || line.starts_with([' ', '+', '-'])
                || line == "@@"
                || line.starts_with("@@ ")
                || line == "*** End of File"
        }
        Some(Section::Delete) | None => false,
    }
}
