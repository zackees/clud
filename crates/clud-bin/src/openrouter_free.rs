//! `:free` OpenRouter model ids (#1833).
//!
//! OpenRouter publishes each free variant as its own catalog row
//! (`vendor/model:free`, price 0/0). Two things go wrong with them today:
//!
//! - `:free` reads as a cost promise, but nothing checks it: a stale or
//!   mistyped id could bill. [`check`] looks the id up in the offline catalog
//!   ([`crate::openrouter_catalog::catalog_cached_or_embedded`], no egress on
//!   the launch path) and the launch refuses anything it cannot prove free.
//! - Free endpoints are served by providers that may train on prompts. A
//!   workspace guardrail that blocks that leaves the id with zero routable
//!   endpoints, and Claude Code shows a bare `400` on the first turn. A
//!   request cannot opt back in (account privacy settings are the ceiling),
//!   so [`probe`] asks once at launch and turns that refusal into an
//!   actionable error before the session starts.

use std::io::Read;
use std::time::Duration;

use crate::openrouter_catalog::Catalog;

/// The suffix OpenRouter uses for a model's free variant.
pub const FREE_SUFFIX: &str = ":free";

/// Where the user changes the guardrail that blocks free endpoints.
pub const GUARDRAILS_URL: &str = "https://openrouter.ai/workspaces/default/guardrails";

const PROBE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// What the offline catalog says about a wire id.
#[derive(Debug, Clone, PartialEq)]
pub enum FreeCheck {
    /// The id does not ask for a free variant; nothing is checked.
    NotRequested,
    /// A catalog row with zero input and output price.
    Free,
    /// A catalog row that is priced (USD per million tokens).
    NotFree { input: f64, output: f64 },
    /// No catalog row matches the id exactly.
    Unknown,
}

impl FreeCheck {
    /// The `free_check` value `--dry-run` reports.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Free => "free",
            Self::NotFree { .. } => "not_free",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify `wire_id` against `catalog`. Exact match only, like
/// [`Catalog::model_by_id`]: a same-family paid row never vouches for a free id.
pub fn check(wire_id: &str, catalog: &Catalog) -> FreeCheck {
    if !wire_id.ends_with(FREE_SUFFIX) {
        return FreeCheck::NotRequested;
    }
    let Some(model) = catalog.model_by_id(wire_id) else {
        return FreeCheck::Unknown;
    };
    let input = model.input_price_per_token.unwrap_or(0.0);
    let output = model.output_price_per_token.unwrap_or(0.0);
    if input == 0.0 && output == 0.0 {
        FreeCheck::Free
    } else {
        FreeCheck::NotFree {
            input: input * 1_000_000.0,
            output: output * 1_000_000.0,
        }
    }
}

/// The launch refusal for a `:free` id the catalog cannot prove free, or
/// `None` when the launch may proceed. A stale catalog only ever refuses.
pub fn refusal(wire_id: &str, check: &FreeCheck, catalog: &Catalog) -> Option<String> {
    match check {
        FreeCheck::NotRequested | FreeCheck::Free => None,
        FreeCheck::Unknown => Some(format!(
            "`{wire_id}` is not in OpenRouter's model catalog, so clud cannot confirm it is \
             free; refusing rather than risk billing. Without `{FREE_SUFFIX}`, {}.",
            paid_alternative(wire_id, catalog)
        )),
        FreeCheck::NotFree { input, output } => Some(format!(
            "`{wire_id}` is priced at ${input:.2}/${output:.2} per million input/output tokens \
             on OpenRouter, but `{FREE_SUFFIX}` asks for a free model; refusing."
        )),
    }
}

/// What dropping `:free` would cost, so no message nudges a user onto a paid
/// model without naming its price (#1833: that nudge is how money was spent).
pub fn paid_alternative(wire_id: &str, catalog: &Catalog) -> String {
    let paid = wire_id.trim_end_matches(FREE_SUFFIX);
    match catalog.model_by_id(paid) {
        Some(model) => format!(
            "the paid `{paid}` bills ${:.2}/${:.2} per million input/output tokens",
            model.input_price_per_token.unwrap_or(0.0) * 1_000_000.0,
            model.output_price_per_token.unwrap_or(0.0) * 1_000_000.0
        ),
        None => format!("`{paid}` is a paid model whose price clud does not know"),
    }
}

/// The one launch line naming what an OpenRouter launch will bill, from the
/// offline catalog; `source` is the selection's `model_source`. `None` when
/// the catalog has no row (an arbitrary pass-through id).
pub fn price_notice(wire_id: &str, catalog: &Catalog, source: &str) -> Option<String> {
    let model = catalog.model_by_id(wire_id)?;
    let input = model.input_price_per_token.unwrap_or(0.0) * 1_000_000.0;
    let output = model.output_price_per_token.unwrap_or(0.0) * 1_000_000.0;
    let price = if input == 0.0 && output == 0.0 {
        "free".to_string()
    } else {
        format!("${input:.2}/${output:.2} per million input/output tokens")
    };
    let origin = match source {
        "cli" => "named on the command line".to_string(),
        "provider_setting" => "saved default from an earlier --model; pass --model to change it"
            .to_string(),
        other => other.replace('_', " "),
    };
    Some(format!("[clud] OpenRouter model {wire_id}: {price} ({origin})"))
}

/// How a launch probe of a free id ended.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeVerdict {
    /// The id answered, or the probe could not tell (network, rate limit,
    /// server error): the launch proceeds and the harness reports any error.
    Proceed,
    /// OpenRouter refused every endpoint for the id; carries the message.
    Blocked(String),
}

/// Map a probe's HTTP status and error body to a verdict. Only an
/// endpoint-exclusion refusal blocks: a free model's 429 or a 5xx says
/// nothing about whether the id can be served.
pub fn classify(wire_id: &str, status: u16, body: &str, paid_note: &str) -> ProbeVerdict {
    let lower = body.to_ascii_lowercase();
    if lower.contains("guardrail") || lower.contains("data policy") {
        return ProbeVerdict::Blocked(format!(
            "OpenRouter excluded every endpoint for `{wire_id}`: free endpoints are served by \
             providers that may train on prompts, and your workspace guardrail (\"Free model \
             training\") or privacy settings block them. Allow it at {GUARDRAILS_URL}. Without \
             `{FREE_SUFFIX}`, {paid_note}."
        ));
    }
    if status == 404 && lower.contains("no endpoints") {
        return ProbeVerdict::Blocked(format!(
            "OpenRouter has no endpoint serving `{wire_id}` right now. Without \
             `{FREE_SUFFIX}`, {paid_note}."
        ));
    }
    ProbeVerdict::Proceed
}

/// Send one `max_tokens: 1` request for `wire_id` with the vault `key`.
/// Runs only for ids [`check`] proved free, so it never bills.
pub fn probe(wire_id: &str, key: &str, paid_note: &str) -> ProbeVerdict {
    let agent = ureq::AgentBuilder::new()
        .timeout(PROBE_TIMEOUT)
        .redirects(0)
        .build();
    let body = serde_json::json!({
        "model": wire_id,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "ping"}],
    });
    match agent
        .post(PROBE_URL)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
    {
        Err(ureq::Error::Status(status, response)) => {
            let mut text = String::new();
            let _ = response.into_reader().take(8192).read_to_string(&mut text);
            classify(wire_id, status, &text, paid_note)
        }
        _ => ProbeVerdict::Proceed,
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;

    pub(super) fn catalog() -> Catalog {
        let row = |id: &str, input: f64, output: f64| {
            serde_json::json!({
                "id": id,
                "name": id,
                "provider": "nvidia",
                "context_length": 262_144,
                "input_price_per_token": input,
                "output_price_per_token": output,
                "cached_input_price_per_token": null,
                "supports_tools": true,
                "supports_text_input": true,
                "supports_text_output": true,
                "supports_reasoning": true,
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
                row("nvidia/nemotron-3-ultra-550b-a55b:free", 0.0, 0.0),
                row("nvidia/nemotron-3-ultra-550b-a55b", 5e-7, 2.2e-6),
                row("acme/priced:free", 1e-6, 2e-6),
            ],
        });
        Catalog::parse(json.to_string().as_bytes()).expect("fixture catalog parses")
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::catalog;
    use super::*;

    const FREE: &str = "nvidia/nemotron-3-ultra-550b-a55b:free";

    #[test]
    fn only_free_suffixed_ids_are_checked() {
        let catalog = catalog();
        assert_eq!(
            check("nvidia/nemotron-3-ultra-550b-a55b", &catalog),
            FreeCheck::NotRequested
        );
        assert_eq!(check("not/in-catalog", &catalog), FreeCheck::NotRequested);
        assert_eq!(
            refusal("not/in-catalog", &FreeCheck::NotRequested, &catalog),
            None
        );
    }

    #[test]
    fn a_zero_priced_free_row_is_allowed() {
        let catalog = catalog();
        let verdict = check(FREE, &catalog);
        assert_eq!(verdict, FreeCheck::Free);
        assert_eq!(refusal(FREE, &verdict, &catalog), None);
    }

    #[test]
    fn a_priced_or_unknown_free_id_is_refused() {
        let catalog = catalog();
        let priced = check("acme/priced:free", &catalog);
        assert_eq!(
            priced,
            FreeCheck::NotFree {
                input: 1.0,
                output: 2.0
            }
        );
        let message =
            refusal("acme/priced:free", &priced, &catalog).expect("priced :free refused");
        assert!(message.contains("$1.00/$2.00"), "{message}");
        let unknown = check("acme/missing:free", &catalog);
        assert_eq!(unknown, FreeCheck::Unknown);
        let message =
            refusal("acme/missing:free", &unknown, &catalog).expect("unknown :free refused");
        assert!(message.contains("`acme/missing` is a paid model"), "{message}");
    }

    #[test]
    fn the_guardrail_refusal_blocks_with_the_fix() {
        let body = r#"{"error":{"code":400,"message":"0 endpoints out of 1 requested are available matching your guardrail restrictions and data policy. Free model training violation (guardrail): 1 endpoint excluded"}}"#;
        let note = paid_alternative(FREE, &catalog());
        assert!(note.contains("$0.50/$2.20"), "{note}");
        let ProbeVerdict::Blocked(message) = classify(FREE, 400, body, &note) else {
            panic!("the guardrail refusal must block");
        };
        assert!(message.contains(GUARDRAILS_URL), "{message}");
        assert!(
            message.contains("`nvidia/nemotron-3-ultra-550b-a55b`"),
            "{message}"
        );
    }

    #[test]
    fn rate_limits_and_server_errors_never_block() {
        for (status, body) in [
            (
                429,
                r#"{"error":{"message":"Rate limit exceeded: free-models-per-min"}}"#,
            ),
            (502, "bad gateway"),
            (400, r#"{"error":{"message":"max_tokens too small"}}"#),
        ] {
            assert_eq!(
                classify(FREE, status, body, ""),
                ProbeVerdict::Proceed,
                "{status}"
            );
        }
        let no_endpoint = r#"{"error":{"message":"No endpoints found for x:free."}}"#;
        assert!(matches!(
            classify(FREE, 404, no_endpoint, ""),
            ProbeVerdict::Blocked(_)
        ));
    }
}

#[cfg(test)]
mod notice_tests {
    use super::tests_support::catalog;
    use super::*;

    #[test]
    fn every_catalogued_launch_names_its_price_and_origin() {
        let catalog = catalog();
        let paid = price_notice("nvidia/nemotron-3-ultra-550b-a55b", &catalog, "provider_setting")
            .expect("catalogued");
        assert!(paid.contains("$0.50/$2.20"), "{paid}");
        assert!(paid.contains("saved default"), "{paid}");
        let free = price_notice(
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            &catalog,
            "cli",
        )
        .expect("catalogued");
        assert!(free.contains(": free (named on the command line)"), "{free}");
        assert_eq!(price_notice("not/in-catalog", &catalog, "cli"), None);
    }
}
