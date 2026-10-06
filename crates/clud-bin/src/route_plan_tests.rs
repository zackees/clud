//! `route_plan` tests: #1852's A1-A8, plus the secret-free guarantee (C12).
//!
//! Every test here is pure: no network, no native vault, no child process. The
//! catalog and the per-turn-effort gate are passed in, and the one piece of
//! process state the precedence rule reads arrives as an explicit [`Ambient`].

use super::*;
use crate::backend::{Backend, HarnessSelection, ModelProvider, PreferenceSource, RoutingMode};
use crate::command::LaunchPlan;

fn plan(provider: ModelProvider, harness: Backend) -> LaunchPlan {
    LaunchPlan {
        unsafe_mode: false,
        command: vec![harness.executable_name().to_string()],
        iterations: 1,
        backend: harness,
        routing_mode: RoutingMode::Direct,
        model_provider: Some(provider),
        requested_harness: Some(match harness {
            Backend::Claude => HarnessSelection::Claude,
            Backend::Codex => HarnessSelection::Codex,
            Backend::DeepSeek => HarnessSelection::DeepSeek,
        }),
        effective_harness: Some(harness),
        provider_source: Some(PreferenceSource::Cli),
        harness_source: Some(PreferenceSource::Cli),
        launch_mode: crate::backend::LaunchMode::Subprocess,
        cwd: None,
        graphics: crate::graphics::GraphicsConfig::default(),
        repeat_schedule: None,
        task_summary: None,
        loop_markers: None,
        stream_json_progress: false,
        codex_model: None,
        model_selection: None,
        failover: None,
        failover_allow_metered: false,
        allowed_models: Vec::new(),
        provider_only: Vec::new(),
        pinned_from_previous_selection: false,
        coauthor: crate::attribution::Coauthor::default(),
        route: None,
    }
}

fn unified(provider: ModelProvider) -> LaunchPlan {
    let mut plan = plan(provider, Backend::Claude);
    plan.routing_mode = RoutingMode::Unified;
    plan
}

fn with_selection(mut plan: LaunchPlan, model: &str, provider: ModelProvider) -> LaunchPlan {
    plan.model_selection =
        crate::provider_catalog::resolve(Some(provider), Some(model), None, None)
            .expect("fixture model resolves");
    plan
}

/// The offline catalog fixture: `openrouter_free` and the per-turn capability
/// both read pricing/capability from here, so neither test touches the daemon's
/// cache or the network.
fn catalog() -> crate::openrouter_catalog::Catalog {
    let row = |id: &str, effort: bool| {
        serde_json::json!({
            "id": id,
            "name": id,
            "provider": "nvidia",
            "context_length": 262_144,
            "input_price_per_token": 1e-6,
            "output_price_per_token": 2e-6,
            "cached_input_price_per_token": null,
            "supports_tools": true,
            "supports_text_input": true,
            "supports_text_output": true,
            "supports_reasoning": true,
            "supports_reasoning_effort": effort,
            "supports_vision": false,
            "eligible_for_coding": true,
            "ineligibility_reasons": [],
        })
    };
    let json = serde_json::json!({
        "schema_version": 1,
        "generated_at": "2026-10-05T00:00:00Z",
        "source": "https://openrouter.ai/api/v1/models",
        "models": [
            row("~anthropic/claude-sonnet-latest", true),
            row("nvidia/nemotron-3-ultra-550b-a55b:free", false),
        ],
    });
    crate::openrouter_catalog::Catalog::parse(json.to_string().as_bytes())
        .expect("fixture catalog parses")
}

fn resolve(plan: &LaunchPlan, ambient: &Ambient) -> ResolvedRoute {
    resolve_with_catalog(plan, ambient, &catalog(), true)
}

fn slot_wire(slot: &Option<SlotModel>) -> Option<&str> {
    slot.as_ref().map(|slot| slot.wire_id.as_str())
}

/// A1: the backend classification covers the whole matrix, and the runtime's
/// own branch order is what decides it.
#[test]
fn every_plan_shape_resolves_to_the_backend_that_will_run_it() {
    let cases = [
        (
            plan(ModelProvider::Claude, Backend::Claude),
            RouteBackend::Native,
        ),
        (
            plan(ModelProvider::DeepSeek, Backend::Claude),
            RouteBackend::Direct,
        ),
        (
            plan(ModelProvider::Kimi, Backend::Claude),
            RouteBackend::Direct,
        ),
        (
            plan(ModelProvider::OpenRouter, Backend::Claude),
            RouteBackend::Direct,
        ),
        (
            plan(ModelProvider::Codex, Backend::Claude),
            RouteBackend::CodexBridge,
        ),
        (
            plan(ModelProvider::DeepSeek, Backend::DeepSeek),
            RouteBackend::DeepSeekNative,
        ),
        (unified(ModelProvider::Claude), RouteBackend::Unified),
        (unified(ModelProvider::OpenRouter), RouteBackend::Unified),
    ];
    for (plan, expected) in cases {
        let route = resolve(&plan, &Ambient::default());
        assert_eq!(route.backend, expected, "{}", route.backend.as_str());
    }
}

/// A2: slot pinning is decided once. A pin covers every slot and fable too,
/// and an ambient subagent wins only when the allowlist admits it -- the
/// single precedence rule #1257 established, here and nowhere else.
#[test]
fn a_pin_covers_every_slot_and_the_ambient_subagent_wins_only_inside_it() {
    let mut pinned = plan(ModelProvider::DeepSeek, Backend::Claude);
    pinned.allowed_models = vec!["deepseek-v4-pro".to_string()];

    let plain = resolve(&pinned, &Ambient::default());
    for slot in [
        &plain.slots.main,
        &plain.slots.opus,
        &plain.slots.sonnet,
        &plain.slots.haiku,
        &plain.slots.fable,
        &plain.slots.subagent,
    ] {
        assert_eq!(slot_wire(slot), Some("deepseek-v4-pro"));
    }

    let admitted = resolve(
        &pinned,
        &Ambient {
            subagent_model: Some("deepseek-v4-pro".to_string()),
        },
    );
    assert_eq!(slot_wire(&admitted.slots.subagent), Some("deepseek-v4-pro"));
    assert_eq!(slot_wire(&admitted.slots.opus), Some("deepseek-v4-pro"));

    // The same rule, outside the boundary: the pin wins the slot back.
    let refused = resolve(
        &pinned,
        &Ambient {
            subagent_model: Some("deepseek-flash".to_string()),
        },
    );
    assert_eq!(slot_wire(&refused.slots.subagent), Some("deepseek-v4-pro"));

    // And on an unconstrained launch there is nothing for the ambient value to
    // win: the descriptor's own role mapping stays authoritative.
    let free = plan(ModelProvider::DeepSeek, Backend::Claude);
    let unconstrained = resolve(
        &free,
        &Ambient {
            subagent_model: Some("some/ambient-model".to_string()),
        },
    );
    assert_eq!(unconstrained.allowlist, Vec::<String>::new());
    assert_eq!(
        slot_wire(&unconstrained.slots.subagent),
        Some("deepseek-flash[1m]")
    );
}

/// A2 also covers the direct-vs-unified equality divergence #1 is about: the
/// *decision* is one, and only the id namespace each renderer asks for differs.
#[test]
fn direct_and_unified_choose_the_same_rows_for_the_same_pin() {
    let mut direct = plan(ModelProvider::OpenRouter, Backend::Claude);
    direct.allowed_models = vec!["openrouter-claude-sonnet".to_string()];
    let mut unified = direct.clone();
    unified.routing_mode = RoutingMode::Unified;

    let direct = resolve(&direct, &Ambient::default());
    let unified = resolve(&unified, &Ambient::default());
    assert_eq!(direct.slots, unified.slots);
    assert_eq!(
        direct
            .slots
            .opus
            .as_ref()
            .and_then(|slot| slot.discovery_id.as_deref()),
        Some("clud-claude-openrouter-sonnet")
    );
}

/// A3: unpinned, every descriptor's role table reaches the slots, and the
/// served subagent model overrides the compiled-in one.
#[test]
fn unpinned_slots_follow_each_descriptor_role_table() {
    for descriptor in crate::provider_registry::ANTHROPIC_COMPAT_PROVIDERS {
        let mut plan = plan(descriptor.provider, Backend::Claude);
        plan.model_selection =
            crate::provider_catalog::resolve(Some(descriptor.provider), None, None, None)
                .expect("provider default resolves");
        let route = resolve(&plan, &Ambient::default());
        let main = slot_wire(&route.slots.main).expect("a descriptor has a reviewed default");
        match descriptor.role_models {
            Some(roles) => {
                assert_eq!(slot_wire(&route.slots.opus), Some(roles.opus));
                assert_eq!(slot_wire(&route.slots.sonnet), Some(roles.sonnet));
                assert_eq!(slot_wire(&route.slots.haiku), Some(roles.haiku));
                assert_eq!(slot_wire(&route.slots.fable), roles.fable);
            }
            None => {
                assert_eq!(slot_wire(&route.slots.opus), Some(main));
                assert_eq!(slot_wire(&route.slots.sonnet), Some(main));
                assert_eq!(
                    slot_wire(&route.slots.haiku),
                    Some(descriptor.subagent_wire_id)
                );
                assert_eq!(slot_wire(&route.slots.fable), Some(main));
            }
        }
    }
}

/// A4: the credential is descriptor-derived. No test can make the resolver
/// invent a vault identifier, because it never carries one.
#[test]
fn every_descriptor_provider_resolves_to_its_own_vault_identifiers() {
    for descriptor in crate::provider_registry::ANTHROPIC_COMPAT_PROVIDERS {
        let route = resolve(
            &plan(descriptor.provider, Backend::Claude),
            &Ambient::default(),
        );
        let upstream = route
            .upstreams
            .first()
            .expect("a direct route has one upstream");
        assert_eq!(
            upstream.credential,
            CredentialSource::Vault(descriptor.provider)
        );
        assert_eq!(
            upstream.credential.vault_identifiers(),
            Some((descriptor.vault_service, descriptor.vault_account))
        );
    }
}

/// A5: one route mapping, total over providers.
#[test]
fn every_provider_maps_to_exactly_one_conversation_route() {
    let mut seen = Vec::new();
    for provider in ModelProvider::ALL {
        let route = conversation_route(*provider);
        assert!(
            !seen.contains(&route),
            "{} and another provider share {:?}",
            provider.as_str(),
            route
        );
        seen.push(route);
    }
    assert_eq!(seen.len(), ModelProvider::ALL.len());
}

/// A6: the context window is keyed on the wire id, so the same id yields the
/// same value on every backend (divergence #3), and an unknown id yields none.
#[test]
fn the_context_window_is_keyed_on_the_wire_id_on_every_backend() {
    let direct = resolve(
        &plan(ModelProvider::DeepSeek, Backend::Claude),
        &Ambient::default(),
    );
    assert_eq!(direct.context.auto_compact_window, Some(786_432));
    assert_eq!(direct.context.max_tokens, None);

    let unified = resolve(&unified(ModelProvider::DeepSeek), &Ambient::default());
    assert_eq!(unified.context, direct.context);

    let unknown = context_policy(Some("vendor/not-in-the-catalog"));
    assert_eq!(unknown, ContextPolicy::default());
}

/// A7: the per-turn capability is a function of the catalog row, so it is the
/// same on the direct and the unified route for the same wire id.
#[test]
fn the_per_turn_capability_is_identical_on_direct_and_unified() {
    let mut direct = with_selection(
        plan(ModelProvider::OpenRouter, Backend::Claude),
        "openrouter-claude-sonnet",
        ModelProvider::OpenRouter,
    );
    direct.allowed_models = vec!["openrouter-claude-sonnet".to_string()];
    let mut unified = direct.clone();
    unified.routing_mode = RoutingMode::Unified;

    let direct = resolve(&direct, &Ambient::default());
    let unified = resolve(&unified, &Ambient::default());
    assert_eq!(
        direct.slots.main.as_ref().unwrap().wire_id,
        "~anthropic/claude-sonnet-latest"
    );
    assert_eq!(direct.effort, unified.effort);
    assert_eq!(direct.slots, unified.slots);

    // The gate is off by default upstream; with it off nothing is advertised.
    let gated_off = resolve_with_catalog(
        &plan(ModelProvider::OpenRouter, Backend::Claude),
        &Ambient::default(),
        &catalog(),
        false,
    );
    assert_eq!(gated_off.effort.per_turn_capability, None);
}

/// A8: resolving twice from the same inputs gives equal values, and the route
/// survives the JSON round trip a daemon worker performs.
#[test]
fn resolving_is_deterministic_and_survives_the_plan_round_trip() {
    let plan = with_selection(
        unified(ModelProvider::OpenRouter),
        "openrouter-claude-sonnet",
        ModelProvider::OpenRouter,
    );
    let first = resolve(&plan, &Ambient::default());
    let second = resolve(&plan, &Ambient::default());
    assert_eq!(first, second);

    let mut carried = plan.clone();
    carried.route = Some(first.clone());
    let json = serde_json::to_string(&carried).expect("a plan serializes");
    let restored: LaunchPlan = serde_json::from_str(&json).expect("a plan round trips");
    assert_eq!(restored.route, Some(first));
}

/// A pre-#1855 payload has no `route` key at all; it must still parse.
#[test]
fn a_plan_without_a_route_still_parses() {
    let mut plan = plan(ModelProvider::Claude, Backend::Claude);
    plan.route = None;
    let json = serde_json::to_string(&plan).expect("a plan serializes");
    assert!(!json.contains("\"route\""), "{json}");
    let restored: LaunchPlan = serde_json::from_str(&json).expect("a pre-#1855 plan parses");
    assert_eq!(restored.route, None);
}

/// C12: a route names where a credential comes from and never what it is. The
/// serialized credential is a provider name, so no field of a route can carry
/// a key even by accident.
#[test]
fn every_route_serializes_its_credentials_as_names_never_secrets() {
    let route = resolve(
        &plan(ModelProvider::OpenRouter, Backend::Claude),
        &Ambient::default(),
    );
    assert_eq!(
        serde_json::to_string(&route.upstreams[0].credential).expect("a credential serializes"),
        r#"{"source":"vault","value":"openrouter"}"#
    );
    let unified = resolve(&unified(ModelProvider::Claude), &Ambient::default());
    let rendered = serde_json::to_string(&unified).expect("a route serializes");
    for needle in [
        "api-key-v1",
        "clud.openrouter",
        "clud.deepseek",
        "clud.kimi",
    ] {
        assert!(!rendered.contains(needle), "{needle} reached {rendered}");
    }
}

/// The applicability table, asserted exhaustively: a new backend that forgets
/// to classify a check fails here.
#[test]
fn every_check_is_classified_for_every_backend() {
    let plans = [
        (
            plan(ModelProvider::DeepSeek, Backend::Claude),
            RouteBackend::Direct,
        ),
        (
            plan(ModelProvider::OpenRouter, Backend::Claude),
            RouteBackend::Direct,
        ),
        (unified(ModelProvider::OpenRouter), RouteBackend::Unified),
        (
            plan(ModelProvider::Codex, Backend::Claude),
            RouteBackend::CodexBridge,
        ),
        (
            plan(ModelProvider::DeepSeek, Backend::DeepSeek),
            RouteBackend::DeepSeekNative,
        ),
        (
            plan(ModelProvider::Claude, Backend::Claude),
            RouteBackend::Native,
        ),
    ];
    for (plan, expected) in plans {
        let route = resolve(&plan, &Ambient::default());
        assert_eq!(route.backend, expected);
        let names: Vec<&str> = route
            .checks
            .iter()
            .map(|check| check.name.as_str())
            .collect();
        assert_eq!(names, vec!["openrouter_free_cost", "provider_only"]);
        for check in &route.checks {
            assert!(!check.verdict.is_empty(), "{} has no verdict", check.name);
        }
    }

    // `--provider-only` is where it is meaningful and refused where it is not.
    let mut refused = plan(ModelProvider::Codex, Backend::Claude);
    refused.provider_only = vec!["deepinfra".to_string()];
    let route = resolve(&refused, &Ambient::default());
    let check = route
        .checks
        .iter()
        .find(|check| check.name == "provider_only")
        .expect("the check is classified");
    assert!(matches!(check.applicability, Applicability::Unsupported(_)));
    assert!(check.verdict.contains("refused"), "{}", check.verdict);
}

/// The `:free` verdict is catalog data, and the route says where it ran.
#[test]
fn a_free_wire_id_is_classified_from_the_offline_catalog() {
    let mut plan = with_selection(
        plan(ModelProvider::OpenRouter, Backend::Claude),
        "acme/missing:free",
        ModelProvider::OpenRouter,
    );
    plan.model_selection.as_mut().unwrap().wire_model = Some("acme/missing:free".to_string());
    let route = resolve(&plan, &Ambient::default());
    let check = route
        .checks
        .iter()
        .find(|check| check.name == "openrouter_free_cost")
        .expect("the check is classified");
    assert_eq!(check.applicability, Applicability::Applies);
    assert!(check.verdict.contains("unknown"), "{}", check.verdict);
}

/// The scrub list each backend owns, including the prefix rule.
#[test]
fn each_backend_scrubs_the_keys_it_owns() {
    let direct = resolve(
        &plan(ModelProvider::OpenRouter, Backend::Claude),
        &Ambient::default(),
    );
    assert!(direct.scrub.iter().any(|key| key == "OPENROUTER_API_KEY"));
    assert!(direct.scrub.iter().any(|key| key == "ANTHROPIC_API_KEY"));
    assert!(!direct
        .scrub
        .iter()
        .any(|key| key == "CLAUDE_CODE_EFFORT_LEVEL"));

    let unified = resolve(&unified(ModelProvider::Claude), &Ambient::default());
    assert!(unified.scrub.iter().any(|key| key == "ANTHROPIC_BASE_URL"));
    assert!(
        !unified
            .scrub
            .iter()
            .any(|key| key == "ANTHROPIC_AUTH_TOKEN"),
        "the gateway keeps the Claude credential so native auth reaches the upstream"
    );

    let native = resolve(
        &plan(ModelProvider::Claude, Backend::Claude),
        &Ambient::default(),
    );
    assert_eq!(native.scrub, Vec::<String>::new());
}

/// The unified route lists every upstream it may serve, and the direct route
/// exactly one. Which of them can answer right now is a launch-time credential
/// probe, deliberately not a resolution input.
#[test]
fn the_unified_route_lists_a_credential_source_per_upstream() {
    let route = resolve(&unified(ModelProvider::Claude), &Ambient::default());
    let providers: Vec<ModelProvider> = route
        .upstreams
        .iter()
        .map(|upstream| upstream.provider)
        .collect();
    assert_eq!(
        providers,
        vec![
            ModelProvider::Claude,
            ModelProvider::DeepSeek,
            ModelProvider::Kimi,
            ModelProvider::OpenRouter,
            ModelProvider::Codex,
        ]
    );
    assert_eq!(route.upstreams[0].credential, CredentialSource::Harness);
    assert_eq!(route.upstreams[4].credential, CredentialSource::CodexAuth);
    for (index, upstream) in route.upstreams.iter().enumerate() {
        assert_eq!(
            upstream.conversation_route,
            conversation_route(upstream.provider),
            "upstream {index} disagrees with the one route mapping"
        );
    }
}

/// The DeepSeek-native harness keeps its key in the environment, and the route
/// says so explicitly instead of hiding it behind a vault id.
#[test]
fn the_deepseek_native_route_names_its_environment_credential() {
    let route = resolve(
        &plan(ModelProvider::DeepSeek, Backend::DeepSeek),
        &Ambient::default(),
    );
    assert_eq!(route.credential_env(), Some("DEEPSEEK_API_KEY"));

    let openrouter = resolve(
        &plan(ModelProvider::OpenRouter, Backend::DeepSeek),
        &Ambient::default(),
    );
    assert_eq!(openrouter.credential_env(), Some("OPENROUTER_API_KEY"));
}
