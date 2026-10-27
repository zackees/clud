//! `clud serve` — a durable, independently-runnable Anthropic Messages API
//! gateway (#2218).
//!
//! Unlike the launch-scoped `--unified` gateway
//! (`docs/architecture/unified-gateway.md`), this server owns its own
//! lifecycle: it launches no harness child and does not exit with one. It
//! reuses the same route table (`provider_registry` + `provider_catalog`) and
//! the same request path as the unified gateway, but binds a caller-chosen
//! loopback address with a client token a caller can hold across runs.
//!
//! First slice: only the vault-backed Anthropic-compatible providers
//! (DeepSeek, Kimi, OpenRouter) are selectable. Native Claude is deferred —
//! clud does not own a Claude credential, and issue #2218 records that as an
//! open design question ("Claude authentication is externally managed").
//! Multi-account balancing and remote (non-loopback) hosting are explicitly
//! out of this slice too.

use crate::args::ServeOptions;
use crate::backend::ModelProvider;
use crate::codex_bridge::{BridgeConfig, BridgeHandle, UnifiedGatewayConfig};
use crate::provider_auth::SecretStore as _;
use base64::Engine as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Providers a `clud serve` invocation can route to in this slice, in a stable
/// order. `--claude` is deliberately excluded (see the module docs).
pub fn selected_providers(options: &ServeOptions) -> Vec<ModelProvider> {
    let mut providers = Vec::new();
    if options.deepseek {
        providers.push(ModelProvider::DeepSeek);
    }
    if options.kimi {
        providers.push(ModelProvider::Kimi);
    }
    if options.openrouter {
        providers.push(ModelProvider::OpenRouter);
    }
    providers
}

/// The provider's credential from clud's own store: the OS vault first, then
/// the owner-only credential file, then the provider's API-key environment
/// variable (`NativeSecretStore`'s chain). Never logged and never returned to
/// a client.
fn provider_key(provider: ModelProvider) -> Option<String> {
    let descriptor = crate::provider_registry::descriptor_for(provider)?;
    crate::provider_auth::NativeSecretStore::new_for(
        descriptor.vault_service,
        descriptor.vault_account,
    )
    .ok()
    .and_then(|store| store.get().ok().flatten())
    .filter(|key| !key.trim().is_empty())
}

/// Build the route table for a served gateway: one Anthropic-compatible
/// provider per requested flag, each backed by its own stored credential.
pub fn build_unified_config(options: &ServeOptions) -> Result<UnifiedGatewayConfig, String> {
    let providers = selected_providers(options);
    if providers.is_empty() {
        return Err(
            "select at least one provider, e.g. `clud serve --openrouter` \
             (`--claude` is not yet supported; see issue #2218)"
                .to_string(),
        );
    }
    let mut config = UnifiedGatewayConfig::new(None, false);
    for provider in providers {
        let key = provider_key(provider).ok_or_else(|| {
            let descriptor = crate::provider_registry::descriptor_for(provider)
                .expect("a selectable provider has a descriptor");
            format!(
                "no {} credential found; run `{}`",
                descriptor.display_name, descriptor.login_command
            )
        })?;
        config = config.with_route(provider, Some(key));
    }
    Ok(config)
}

/// Loopback-only in this slice: a long-lived server has a different trust
/// boundary from the ephemeral child-only bridge, and #2218 keeps remote
/// hosting and its access story out of the first release.
pub fn parse_bind(bind: &str, port: u16) -> Result<SocketAddr, String> {
    let ip: IpAddr = bind
        .parse()
        .map_err(|_| format!("invalid --bind address: {bind}"))?;
    if !ip.is_loopback() {
        return Err(format!(
            "clud serve binds loopback only in this release; refusing non-loopback address {bind}"
        ));
    }
    Ok(SocketAddr::new(ip, port))
}

/// A fresh client bearer for the served listener.
fn new_client_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("failed to create client credentials: {error}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn provider_names(options: &ServeOptions) -> String {
    selected_providers(options)
        .iter()
        .map(|provider| provider.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Run the durable server until Ctrl+C. Returns the process exit code.
pub fn run(options: &ServeOptions) -> i32 {
    if options.claude {
        eprintln!(
            "[clud] serve: --claude is not yet available. clud does not own a Claude \
             credential (see the open question on issue #2218); use \
             `clud serve --openrouter` (or --deepseek/--kimi) today."
        );
        return 2;
    }
    let config = match build_unified_config(options) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("[clud] serve: {message}");
            return 2;
        }
    };
    let bind = match parse_bind(&options.bind, options.port) {
        Ok(bind) => bind,
        Err(message) => {
            eprintln!("[clud] serve: {message}");
            return 2;
        }
    };
    let token = match new_client_token() {
        Ok(token) => token,
        Err(message) => {
            eprintln!("[clud] serve: {message}");
            return 2;
        }
    };
    let bridge_config = BridgeConfig::default().with_unified_gateway(config);
    let mut bridge = match BridgeHandle::start_serving(bridge_config, bind, token) {
        Ok(bridge) => bridge,
        Err(error) => {
            eprintln!("[clud] serve: failed to start the gateway: {error}");
            return 2;
        }
    };
    // The machine-readable line a client (or a test) parses to learn where the
    // server is and how to authenticate. The token is the client credential,
    // never an upstream provider credential.
    println!(
        "[clud] serve: endpoint={} token={} providers={}",
        bridge.base_url(),
        bridge.bearer_token(),
        provider_names(options)
    );
    println!(
        "[clud] serve: listening on {} — send the `{}` header to authenticate; press Ctrl+C to stop",
        bridge.base_url(),
        crate::codex_bridge::UNIFIED_GATEWAY_TOKEN_HEADER,
    );
    let interrupted = crate::startup::install_ctrl_c_flag(false);
    while !interrupted.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Err(error) = bridge.shutdown() {
        eprintln!("[clud] serve: shutdown error: {error}");
        return 1;
    }
    println!("[clud] serve: stopped");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(claude: bool, deepseek: bool, kimi: bool, openrouter: bool) -> ServeOptions {
        ServeOptions {
            claude,
            deepseek,
            kimi,
            openrouter,
            bind: "127.0.0.1".to_string(),
            port: 0,
        }
    }

    #[test]
    fn selection_is_stable_and_flag_driven() {
        assert!(selected_providers(&options(false, false, false, false)).is_empty());
        assert_eq!(
            selected_providers(&options(false, false, false, true)),
            vec![ModelProvider::OpenRouter]
        );
        assert_eq!(
            selected_providers(&options(false, true, true, true)),
            vec![
                ModelProvider::DeepSeek,
                ModelProvider::Kimi,
                ModelProvider::OpenRouter
            ]
        );
    }

    #[test]
    fn no_provider_named_is_an_actionable_error() {
        let error = build_unified_config(&options(false, false, false, false))
            .expect_err("an empty selection must not build a gateway");
        assert!(error.contains("select at least one provider"), "{error}");
        assert!(error.contains("#2218"), "{error}");
    }

    #[test]
    fn claude_is_refused_with_the_issue_reference() {
        // `run` returns before it reads any credential when --claude is set, so
        // this exercises the refusal path without touching the vault.
        assert_eq!(run(&options(true, false, false, false)), 2);
    }

    #[test]
    fn bind_is_loopback_only() {
        assert_eq!(
            parse_bind("127.0.0.1", 8787).expect("loopback is allowed"),
            SocketAddr::new("127.0.0.1".parse().unwrap(), 8787)
        );
        assert!(parse_bind("::1", 0).is_ok(), "IPv6 loopback is allowed too");
        let refused = parse_bind("0.0.0.0", 8787).expect_err("wildcard must be refused");
        assert!(refused.contains("loopback only"), "{refused}");
        assert!(parse_bind("not-an-ip", 0).is_err());
    }
}
