//! Action-first provider credential commands (issue #898).
//!
//! This module owns only the CLI grammar and secret-free aggregate status view.
//! Provider-specific vault and OAuth behavior remains in its existing module.

use std::sync::atomic::AtomicBool;

use crate::args::{AuthProvider, AuthSubcommand, CodexAuthSubcommand, DeepseekAuthSubcommand};
use crate::deepseek_auth::{NativeSecretStore, SecretStore};

pub fn run(subcommand: Option<&AuthSubcommand>, interrupted: &AtomicBool) -> i32 {
    match subcommand {
        Some(AuthSubcommand::Login {
            provider,
            acknowledge_experimental,
            no_browser,
        }) => login(
            *provider,
            *acknowledge_experimental,
            *no_browser,
            interrupted,
        ),
        Some(AuthSubcommand::Status { provider, json }) => status(*provider, *json),
        Some(AuthSubcommand::Logout { provider, json }) => logout(*provider, *json),
        None => {
            println!("Usage: clud auth <login|status|logout> <provider>");
            println!("Providers: codex, deepseek (Claude authentication is externally managed)");
            0
        }
    }
}

fn login(
    provider: AuthProvider,
    acknowledge_experimental: bool,
    no_browser: bool,
    interrupted: &AtomicBool,
) -> i32 {
    match provider {
        AuthProvider::Codex => crate::codex_auth::run(
            &CodexAuthSubcommand::Login {
                acknowledge_experimental,
                no_browser,
            },
            interrupted,
        ),
        AuthProvider::Deepseek => crate::deepseek_auth::run(&DeepseekAuthSubcommand::Login),
        AuthProvider::Claude => externally_managed("login"),
    }
}

fn logout(provider: AuthProvider, json: bool) -> i32 {
    match provider {
        AuthProvider::Codex => crate::codex_auth::run(
            &CodexAuthSubcommand::Logout { json },
            &AtomicBool::new(false),
        ),
        AuthProvider::Deepseek => {
            crate::deepseek_auth::run(&DeepseekAuthSubcommand::Logout { json })
        }
        AuthProvider::Claude => externally_managed("logout"),
    }
}

fn externally_managed(action: &str) -> i32 {
    eprintln!(
        "Claude authentication is managed by Claude Code; clud cannot {action} Claude credentials"
    );
    2
}

fn status(provider: Option<AuthProvider>, json: bool) -> i32 {
    let providers = match provider {
        Some(provider) => vec![provider],
        None => vec![
            AuthProvider::Claude,
            AuthProvider::Codex,
            AuthProvider::Deepseek,
        ],
    };
    let rows: Vec<serde_json::Value> = providers.into_iter().map(status_row).collect();
    if json {
        println!("{}", serde_json::json!({"providers": rows}));
    } else {
        for row in &rows {
            println!(
                "{:<9} {:<18} {}",
                row["provider"].as_str().unwrap_or("unknown"),
                row["source"].as_str().unwrap_or("unknown"),
                row["status"].as_str().unwrap_or("unknown"),
            );
        }
    }
    if provider.is_some() && !rows[0]["configured"].as_bool().unwrap_or(false) {
        1
    } else {
        0
    }
}

fn status_row(provider: AuthProvider) -> serde_json::Value {
    match provider {
        AuthProvider::Claude => serde_json::json!({
            "provider": provider.as_str(),
            "source": "claude_code",
            "status": "externally_managed",
            "configured": true,
        }),
        AuthProvider::Codex => {
            let configured = dirs::home_dir()
                .and_then(|home| crate::codex_auth::load_at(&home).ok().flatten())
                .is_some()
                || std::env::var("OPENAI_API_KEY")
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty());
            serde_json::json!({
                "provider": provider.as_str(),
                "source": "clud_chatgpt_subscription_or_openai_api_key",
                "status": if configured { "configured" } else { "login_required" },
                "configured": configured,
            })
        }
        AuthProvider::Deepseek => {
            let configured = NativeSecretStore::new()
                .and_then(|store| store.get())
                .ok()
                .flatten()
                .is_some();
            serde_json::json!({
                "provider": provider.as_str(),
                "source": "native_vault",
                "status": if configured { "configured" } else { "login_required" },
                "configured": configured,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_status_is_secret_free_and_externally_managed() {
        let row = status_row(AuthProvider::Claude).to_string();
        assert!(row.contains("externally_managed"));
        assert!(!row.contains("token"));
    }

    #[test]
    fn claude_credential_mutation_is_rejected() {
        assert_eq!(externally_managed("login"), 2);
    }
}
