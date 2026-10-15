//! Windows half of `crate::config`: the default editor.

pub fn default_editor_command() -> Result<Vec<String>, String> {
    Ok(vec!["notepad.exe".to_string()])
}
