//! Unix half of `crate::config`: the default editor.

#[cfg(target_os = "macos")]
pub fn default_editor_command() -> Result<Vec<String>, String> {
    Ok(vec!["open".to_string(), "-t".to_string()])
}

#[cfg(not(target_os = "macos"))]
pub fn default_editor_command() -> Result<Vec<String>, String> {
    for candidate in ["xdg-open", "nano", "vi"] {
        if which::which(candidate).is_ok() {
            return Ok(vec![candidate.to_string()]);
        }
    }
    Err("no editor found; set EDITOR or pass --editor <cmd>".to_string())
}
