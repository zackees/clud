use super::*;
use serde_json::{json, Value};
use tempfile::TempDir;

const VERSION: &str = "1.2.3";

fn run<F, R>(f: F) -> R
where
    F: FnOnce(&Path) -> R,
{
    let tmp = TempDir::new().unwrap();
    f(tmp.path())
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

// ---------- Claude MCP --------------------------------------------------

#[test]
fn ensure_claude_mcp_writes_entry_on_empty_file() {
    run(|home| {
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        let mut out = Vec::new();
        let outcome = ensure_claude_mcp(home, VERSION, &mut out).unwrap();
        assert_eq!(outcome, Outcome::Wrote);
        let json = read_json(&home.join(".claude.json"));
        let block = &json["mcpServers"]["clud-memory"];
        assert_eq!(block["command"], "clud");
        assert_eq!(block["args"], json!(["mcp"]));
        assert_eq!(block["_clud_managed"], json!(true));
        assert_eq!(block["_clud_version"], VERSION);
    });
}

#[test]
fn ensure_claude_mcp_works_on_missing_file() {
    run(|home| {
        let mut out = Vec::new();
        let outcome = ensure_claude_mcp(home, VERSION, &mut out).unwrap();
        assert_eq!(outcome, Outcome::Wrote);
        assert!(home.join(".claude.json").is_file());
    });
}

#[test]
fn mcp_registration_writes_clud_memory_block_idempotently() {
    run(|home| {
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        let mut out = Vec::new();
        assert_eq!(
            ensure_claude_mcp(home, VERSION, &mut out).unwrap(),
            Outcome::Wrote
        );
        out.clear();
        let outcome = ensure_claude_mcp(home, VERSION, &mut out).unwrap();
        assert_eq!(outcome, Outcome::AlreadyPresent);
    });
}

#[test]
fn mcp_registration_refuses_to_clobber_user_block() {
    run(|home| {
        let user = json!({
            "mcpServers": {
                "clud-memory": {
                    "command": "/usr/local/bin/clud-fork",
                    "args": []
                }
            }
        });
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .unwrap();
        let before = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        let mut out = Vec::new();
        let err = ensure_claude_mcp(home, VERSION, &mut out).unwrap_err();
        assert!(matches!(err, Error::UserDefined { .. }));
        let after = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        assert_eq!(before, after, "must not overwrite user-defined block");
    });
}

#[test]
fn ensure_claude_mcp_preserves_other_servers() {
    run(|home| {
        let user = json!({
            "mcpServers": {
                "gmail": { "command": "gmail-mcp", "args": ["--auth"] }
            },
            "otherKey": "value"
        });
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .unwrap();
        let mut out = Vec::new();
        assert_eq!(
            ensure_claude_mcp(home, VERSION, &mut out).unwrap(),
            Outcome::Wrote
        );
        let json = read_json(&home.join(".claude.json"));
        assert_eq!(json["otherKey"], "value");
        assert_eq!(json["mcpServers"]["gmail"]["command"], "gmail-mcp");
        assert_eq!(json["mcpServers"]["clud-memory"]["command"], "clud");
    });
}

#[test]
fn ensure_claude_mcp_rewrites_version_drift() {
    run(|home| {
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        let mut out = Vec::new();
        ensure_claude_mcp(home, "0.0.1", &mut out).unwrap();
        let outcome = ensure_claude_mcp(home, VERSION, &mut out).unwrap();
        assert_eq!(outcome, Outcome::Wrote);
        let json = read_json(&home.join(".claude.json"));
        assert_eq!(json["mcpServers"]["clud-memory"]["_clud_version"], VERSION);
    });
}

// ---------- Codex MCP (TOML) -------------------------------------------

#[test]
fn ensure_codex_mcp_writes_entry_on_empty_file() {
    run(|home| {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            ensure_codex_mcp(home, VERSION, &mut out).unwrap(),
            Outcome::Wrote
        );
        let text = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(text.contains("[mcp_servers.clud-memory]"), "{text}");
        assert!(text.contains("managed-by: clud-memory"), "{text}");
        assert!(text.contains("command = \"clud\""), "{text}");
        assert!(text.contains("\"mcp\""), "{text}");
    });
}

#[test]
fn codex_toml_registration_preserves_existing_keys() {
    run(|home| {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let body = r#"# user comment
model = "gpt-5"

[mcp_servers.foo]
# this is a foo server
command = "foo"
args = ["a", "b"]
"#;
        std::fs::write(home.join(".codex/config.toml"), body).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            ensure_codex_mcp(home, VERSION, &mut out).unwrap(),
            Outcome::Wrote
        );
        let text = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(text.contains("# user comment"), "{text}");
        assert!(text.contains("model = \"gpt-5\""), "{text}");
        assert!(text.contains("[mcp_servers.foo]"), "{text}");
        assert!(text.contains("# this is a foo server"), "{text}");
        assert!(text.contains("[mcp_servers.clud-memory]"), "{text}");
        assert!(text.contains("# managed-by: clud-memory"), "{text}");
    });
}

#[test]
fn ensure_codex_mcp_idempotent() {
    run(|home| {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let mut out = Vec::new();
        ensure_codex_mcp(home, VERSION, &mut out).unwrap();
        let outcome = ensure_codex_mcp(home, VERSION, &mut out).unwrap();
        assert_eq!(outcome, Outcome::AlreadyPresent);
    });
}

#[test]
fn ensure_codex_mcp_refuses_user_block() {
    run(|home| {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let body = "[mcp_servers.clud-memory]\ncommand = \"other\"\nargs = []\n";
        std::fs::write(home.join(".codex/config.toml"), body).unwrap();
        let mut out = Vec::new();
        let err = ensure_codex_mcp(home, VERSION, &mut out).unwrap_err();
        assert!(matches!(err, Error::UserDefined { .. }));
        let after = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert_eq!(after, body);
    });
}

// ---------- Claude hooks ------------------------------------------------

#[test]
fn hook_registration_writes_all_four_entries() {
    run(|home| {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            ensure_claude_hooks(home, VERSION, &mut out).unwrap(),
            Outcome::Wrote
        );
        let json = read_json(&home.join(".claude/settings.json"));
        let hooks = &json["hooks"];
        for (event, command) in HOOK_EVENTS {
            let arr = hooks[event].as_array().expect("event array");
            assert_eq!(arr.len(), 1, "{event}");
            assert_eq!(arr[0]["hooks"][0]["command"], *command);
            assert_eq!(arr[0]["hooks"][0]["timeout"], json!(30));
            assert_eq!(arr[0]["hooks"][0]["_clud_managed"], json!(true));
        }
    });
}

#[test]
fn hook_registration_is_idempotent() {
    run(|home| {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let mut out = Vec::new();
        ensure_claude_hooks(home, VERSION, &mut out).unwrap();
        let outcome = ensure_claude_hooks(home, VERSION, &mut out).unwrap();
        assert_eq!(outcome, Outcome::AlreadyPresent);
    });
}

#[test]
fn hook_registration_refuses_to_clobber_user_entries() {
    run(|home| {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let user = json!({
            "hooks": {
                "SessionStart": [
                    {
                        "hooks": [
                            { "type": "command", "command": "clud hook session-start", "timeout": 60 }
                        ]
                    }
                ]
            }
        });
        std::fs::write(
            home.join(".claude/settings.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .unwrap();
        let before = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        let mut out = Vec::new();
        let err = ensure_claude_hooks(home, VERSION, &mut out).unwrap_err();
        assert!(matches!(err, Error::UserDefined { .. }));
        let after = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert_eq!(before, after);
    });
}

#[test]
fn hook_registration_appends_to_existing_user_hooks() {
    run(|home| {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let user = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "echo pretool" }
                        ]
                    }
                ]
            }
        });
        std::fs::write(
            home.join(".claude/settings.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .unwrap();
        let mut out = Vec::new();
        ensure_claude_hooks(home, VERSION, &mut out).unwrap();
        let json = read_json(&home.join(".claude/settings.json"));
        assert_eq!(
            json["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "echo pretool"
        );
        assert_eq!(
            json["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "clud hook session-start"
        );
    });
}

#[test]
fn ensure_codex_hooks_writes_all_four_entries() {
    run(|home| {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            ensure_codex_hooks(home, VERSION, &mut out).unwrap(),
            Outcome::Wrote
        );
        let json = read_json(&home.join(".codex/hooks.json"));
        for (event, command) in HOOK_EVENTS {
            let arr = json["hooks"][event].as_array().expect("event array");
            assert_eq!(arr.len(), 1, "{event}");
            assert_eq!(arr[0]["hooks"][0]["command"], *command);
        }
    });
}

// ---------- memory_already_registered ----------------------------------

#[test]
fn memory_already_registered_returns_true_after_ensure() {
    run(|home| {
        assert!(!memory_already_registered(home));
        let mut out = Vec::new();
        ensure_claude_mcp(home, VERSION, &mut out).unwrap();
        assert!(memory_already_registered(home));
    });
}

#[test]
fn memory_already_registered_false_for_user_defined_block() {
    run(|home| {
        let user = json!({
            "mcpServers": {
                "clud-memory": { "command": "other", "args": [] }
            }
        });
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .unwrap();
        assert!(!memory_already_registered(home));
    });
}

// ---------- Remove helpers --------------------------------------------

#[test]
fn remove_claude_mcp_strips_managed_entry() {
    run(|home| {
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        let mut out = Vec::new();
        ensure_claude_mcp(home, VERSION, &mut out).unwrap();
        assert_eq!(remove_claude_mcp(home).unwrap(), Outcome::Removed);
        let json = read_json(&home.join(".claude.json"));
        assert!(json["mcpServers"]
            .as_object()
            .map(|m| !m.contains_key("clud-memory"))
            .unwrap_or(true));
    });
}

#[test]
fn remove_returns_not_present_on_missing() {
    run(|home| {
        assert_eq!(remove_claude_mcp(home).unwrap(), Outcome::NotPresent);
        assert_eq!(remove_codex_mcp(home).unwrap(), Outcome::NotPresent);
        assert_eq!(remove_claude_hooks(home).unwrap(), Outcome::NotPresent);
        assert_eq!(remove_codex_hooks(home).unwrap(), Outcome::NotPresent);
    });
}

#[test]
fn remove_claude_hooks_strips_managed_entries_but_preserves_user_hooks() {
    run(|home| {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let user = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{ "type": "command", "command": "echo x" }]
                    }
                ]
            }
        });
        std::fs::write(
            home.join(".claude/settings.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .unwrap();
        let mut out = Vec::new();
        ensure_claude_hooks(home, VERSION, &mut out).unwrap();
        assert_eq!(remove_claude_hooks(home).unwrap(), Outcome::Removed);
        let json = read_json(&home.join(".claude/settings.json"));
        assert_eq!(
            json["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "echo x"
        );
        assert!(json["hooks"]["SessionStart"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true));
    });
}

// ---------- Codex hook integration -------------------------------------

#[test]
fn ensure_codex_hooks_appends_to_existing_user_hooks() {
    run(|home| {
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let user = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{ "type": "command", "command": "do.sh", "timeout": 30 }]
                    }
                ]
            }
        });
        std::fs::write(
            home.join(".codex/hooks.json"),
            serde_json::to_string_pretty(&user).unwrap(),
        )
        .unwrap();
        let mut out = Vec::new();
        ensure_codex_hooks(home, VERSION, &mut out).unwrap();
        let json = read_json(&home.join(".codex/hooks.json"));
        assert_eq!(
            json["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "do.sh"
        );
        for (event, command) in HOOK_EVENTS {
            assert_eq!(
                json["hooks"][event][0]["hooks"][0]["command"], *command,
                "{event}"
            );
        }
    });
}
