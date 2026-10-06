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
pub fn refusal(wire_id: &str, check: &FreeCheck) -> Option<String> {
    let paid = wire_id.trim_end_matches(FREE_SUFFIX);
    match check {
        FreeCheck::NotRequested | FreeCheck::Free => None,
        FreeCheck::Unknown => Some(format!(
            "`{wire_id}` is not in OpenRouter's model catalog, so clud cannot confirm it is \
             free; refusing rather than risk billing. Drop `{FREE_SUFFIX}` (`{paid}`) to use \
             the paid model."
        )),
        FreeCheck::NotFree { input, output } => Some(format!(
            "`{wire_id}` is priced at ${input:.2}/${output:.2} per million input/output tokens \
             on OpenRouter, but `{FREE_SUFFIX}` asks for a free model; refusing."
        )),
    }
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
pub fn classify(wire_id: &str, status: u16, body: &str) -> ProbeVerdict {
    let lower = body.to_ascii_lowercase();
    let paid = wire_id.trim_end_matches(FREE_SUFFIX);
    if lower.contains("guardrail") || lower.contains("data policy") {
        return ProbeVerdict::Blocked(format!(
            "OpenRouter excluded every endpoint for `{wire_id}`: free endpoints are served by \
             providers that may train on prompts, and your workspace guardrail (\"Free model \
             training\") or privacy settings block them. Allow it at {GUARDRAILS_URL}, or drop \
             `{FREE_SUFFIX}` (`{paid}`) to use the paid model."
        ));
    }
    if status == 404 && lower.contains("no endpoints") {
        return ProbeVerdict::Blocked(format!(
            "OpenRouter has no endpoint serving `{wire_id}` right now; drop `{FREE_SUFFIX}` \
             (`{paid}`) to use the paid model."
        ));
    }
    ProbeVerdict::Proceed
}

/// Send one `max_tokens: 1` request for `wire_id` with the vault `key`.
/// Runs only for ids [`check`] proved free, so it never bills.
pub fn probe(wire_id: &str, key: &str) -> ProbeVerdict {
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
        .send_json(body)
    {
        Err(ureq::Error::Status(status, response)) => {
            let mut text = String::new();
            let _ = response.into_reader().take(8192).read_to_string(&mut text);
            classify(wire_id, status, &text)
        }
        _ => ProbeVerdict::Proceed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREE: &str = "nvidia/nemotron-3-ultra-550b-a55b:free";

    fn catalog() -> Catalog {
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
                row(FREE, 0.0, 0.0),
                row("nvidia/nemotron-3-ultra-550b-a55b", 5e-7, 2.2e-6),
                row("acme/priced:free", 1e-6, 2e-6),
            ],
        });
        Catalog::parse(json.to_string().as_bytes()).expect("fixture catalog parses")
    }

    #[test]
    fn only_free_suffixed_ids_are_checked() {
        let catalog = catalog();
        assert_eq!(
            check("nvidia/nemotron-3-ultra-550b-a55b", &catalog),
            FreeCheck::NotRequested
        );
        assert_eq!(check("not/in-catalog", &catalog), FreeCheck::NotRequested);
        assert_eq!(refusal("not/in-catalog", &FreeCheck::NotRequested), None);
    }

    #[test]
    fn a_zero_priced_free_row_is_allowed() {
        let verdict = check(FREE, &catalog());
        assert_eq!(verdict, FreeCheck::Free);
        assert_eq!(refusal(FREE, &verdict), None);
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
        let message = refusal("acme/priced:free", &priced).expect("priced :free refused");
        assert!(message.contains("$1.00/$2.00"), "{message}");
        let unknown = check("acme/missing:free", &catalog);
        assert_eq!(unknown, FreeCheck::Unknown);
        let message = refusal("acme/missing:free", &unknown).expect("unknown :free refused");
        assert!(message.contains("`acme/missing`"), "{message}");
    }

    #[test]
    fn the_guardrail_refusal_blocks_with_the_fix() {
        let body = r#"{"error":{"code":400,"message":"0 endpoints out of 1 requested are available matching your guardrail restrictions and data policy. Free model training violation (guardrail): 1 endpoint excluded"}}"#;
        let ProbeVerdict::Blocked(message) = classify(FREE, 400, body) else {
            panic!("the guardrail refusal must block");
        };
        assert!(message.contains(GUARDRAILS_URL), "{message}");
        assert!(message.contains("`nvidia/nemotron-3-ultra-550b-a55b`"), "{message}");
    }

    #[test]
    fn rate_limits_and_server_errors_never_block() {
        for (status, body) in [
            (429, r#"{"error":{"message":"Rate limit exceeded: free-models-per-min"}}"#),
            (502, "bad gateway"),
            (400, r#"{"error":{"message":"max_tokens too small"}}"#),
        ] {
            assert_eq!(classify(FREE, status, body), ProbeVerdict::Proceed, "{status}");
        }
        let no_endpoint = r#"{"error":{"message":"No endpoints found for x:free."}}"#;
        assert!(matches!(
            classify(FREE, 404, no_endpoint),
            ProbeVerdict::Blocked(_)
        ));
    }
}
