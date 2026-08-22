//! The failover ladder: an ordered, cost-labeled list of routes (#968).
//!
//! The ladder is **configured, never guessed**. Its default holds exactly one
//! rung — the route the launch already selected — so a user who has not asked
//! for failover sees no change in behavior and no change in spend. Extra rungs
//! come from `--failover` or settings, and every rung carries who pays for it,
//! because descending from a subscription onto a metered key is the difference
//! between a free recovery and a surprise invoice. That is why
//! [`CostOwner::Metered`] rungs are skipped unless consent was recorded:
//! automatic recovery must not become an automatic charge.
//!
//! A rung names either a catalog model — resolved to its provider and wire ID —
//! or an ordinary Claude model ID, which is forwarded verbatim. That asymmetry
//! is deliberate and matches the gateway: clud's catalog owns the synthetic
//! provider namespaces, while Anthropic owns its own inventory, so a Claude
//! rung must not be validated against a compile-time list that will age.
//!
//! See [`docs/architecture/provider-failover.md`](../../../docs/architecture/provider-failover.md).

use std::fmt;
use std::time::Instant;

use crate::backend::ModelProvider;
use crate::codex_history::ConversationRoute;
use crate::provider_catalog;
use crate::route_health::RouteLedger;

/// Who pays for a rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostOwner {
    /// Covered by a plan the user already pays for.
    Subscription,
    /// Billed per token against a key or balance.
    Metered,
}

impl CostOwner {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::Metered => "metered",
        }
    }

    /// Reviewed cost owner per provider.
    ///
    /// Claude and Codex are reached through logins clud does not meter; the
    /// Anthropic-compatible providers are reached with a key that bills per
    /// token. A provider added later must make this choice explicitly rather
    /// than inheriting a default, which is why this match is exhaustive.
    pub fn for_provider(provider: ModelProvider) -> Self {
        match provider {
            ModelProvider::Claude | ModelProvider::Codex => Self::Subscription,
            ModelProvider::DeepSeek | ModelProvider::Kimi | ModelProvider::OpenRouter => {
                Self::Metered
            }
        }
    }
}

/// One route the gateway may fall back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverRung {
    /// Exactly what the user wrote, for notices and diagnostics.
    pub spec: String,
    /// Model ID to place in the replayed request body.
    pub wire_id: String,
    pub provider: ModelProvider,
    pub route: ConversationRoute,
    pub cost: CostOwner,
}

impl FailoverRung {
    /// One line for a notice. Names the route and its cost owner, never a
    /// credential.
    pub fn label(&self) -> String {
        format!("{} ({})", self.spec, self.cost.as_str())
    }
}

/// Why a rung could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LadderError {
    /// The spec named a provider the gateway cannot route.
    Unroutable {
        spec: String,
        provider: &'static str,
    },
    /// The spec was empty or whitespace.
    Empty,
}

impl fmt::Display for LadderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unroutable { spec, provider } => write!(
                formatter,
                "`{spec}` resolves to {provider}, which the unified gateway does not route yet"
            ),
            Self::Empty => formatter.write_str("a failover rung cannot be empty"),
        }
    }
}

/// The route a provider is served on inside the gateway, if any.
fn route_for(provider: ModelProvider) -> Option<ConversationRoute> {
    match provider {
        ModelProvider::Claude => Some(ConversationRoute::Claude),
        ModelProvider::Codex => Some(ConversationRoute::Codex),
        ModelProvider::DeepSeek => Some(ConversationRoute::DeepSeek),
        // Direct-launch providers. They have no gateway route to fall back to
        // until they are promoted, and a rung that cannot be served is worse
        // than no rung: it would consume a descent and then fail.
        ModelProvider::Kimi | ModelProvider::OpenRouter => None,
    }
}

/// An ordered list of fallback routes plus the consent that governs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailoverLadder {
    rungs: Vec<FailoverRung>,
    allow_metered: bool,
}

impl FailoverLadder {
    /// Parse a comma-separated ladder such as `claude-opus-4-1,codex-terra`.
    ///
    /// Order is significant and preserved: it is the descent order, and the
    /// caller is expected to put same-family rungs first because a switch
    /// inside one provider family costs only a cold cache, while crossing
    /// families additionally costs a reseed.
    pub fn parse(spec: &str, allow_metered: bool) -> Result<Self, LadderError> {
        let mut rungs = Vec::new();
        for raw in spec.split(',') {
            let name = raw.trim();
            if name.is_empty() {
                if spec.trim().is_empty() {
                    return Ok(Self {
                        rungs,
                        allow_metered,
                    });
                }
                return Err(LadderError::Empty);
            }
            rungs.push(Self::rung(name)?);
        }
        Ok(Self {
            rungs,
            allow_metered,
        })
    }

    fn rung(name: &str) -> Result<FailoverRung, LadderError> {
        let catalog = provider_catalog::model_by_cli_id(name)
            .or_else(|| provider_catalog::model_by_wire_id(name));
        match catalog {
            // A catalog row that is not Claude names a provider namespace clud
            // owns, so the replayed body must carry that row's wire ID.
            Some(entry) if entry.provider != ModelProvider::Claude => {
                let Some(route) = route_for(entry.provider) else {
                    return Err(LadderError::Unroutable {
                        spec: name.to_string(),
                        provider: entry.provider.as_str(),
                    });
                };
                Ok(FailoverRung {
                    spec: name.to_string(),
                    wire_id: entry.wire_id.to_string(),
                    provider: entry.provider,
                    route,
                    cost: CostOwner::for_provider(entry.provider),
                })
            }
            // Everything else is an ordinary Claude model ID. Anthropic owns
            // its inventory, so it is forwarded exactly as written rather than
            // checked against a list that ages.
            _ => Ok(FailoverRung {
                spec: name.to_string(),
                wire_id: name.to_string(),
                provider: ModelProvider::Claude,
                route: ConversationRoute::Claude,
                cost: CostOwner::Subscription,
            }),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rungs.is_empty()
    }

    pub fn rungs(&self) -> &[FailoverRung] {
        &self.rungs
    }

    pub fn allows_metered(&self) -> bool {
        self.allow_metered
    }

    /// The next rung after `after` that can actually serve right now.
    ///
    /// `after` is the spec of the rung just tried, or `None` to start from the
    /// top. A rung is passed over when the ledger says its route is cooling or
    /// down, and when it is metered without recorded consent — in both cases
    /// descending onto it would spend a request to learn what is already known.
    pub fn next_available(
        &self,
        after: Option<&str>,
        ledger: &RouteLedger,
        now: Instant,
    ) -> Option<&FailoverRung> {
        let start = match after {
            Some(spec) => self
                .rungs
                .iter()
                .position(|rung| rung.spec == spec)
                .map_or(0, |index| index + 1),
            None => 0,
        };
        self.rungs[start.min(self.rungs.len())..]
            .iter()
            .find(|rung| {
                (self.allow_metered || rung.cost == CostOwner::Subscription)
                    && ledger.is_available(rung.route, now)
            })
    }

    /// Rungs that exist but are withheld for want of consent. Reported once so
    /// a user who wanted the fallback learns why it was not taken.
    pub fn withheld_for_consent(&self) -> Vec<&FailoverRung> {
        if self.allow_metered {
            return Vec::new();
        }
        self.rungs
            .iter()
            .filter(|rung| rung.cost == CostOwner::Metered)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_health::RouteVerdict;
    use std::time::Duration;

    #[test]
    fn a_catalog_rung_carries_its_provider_wire_id_and_an_unknown_id_stays_claude() {
        let ladder = FailoverLadder::parse("deepseek-v4-flash,claude-opus-4-1", true).unwrap();
        let rungs = ladder.rungs();
        assert_eq!(rungs[0].provider, ModelProvider::DeepSeek);
        assert_eq!(rungs[0].route, ConversationRoute::DeepSeek);
        assert_eq!(rungs[0].wire_id, "deepseek-v4-flash");
        assert_eq!(rungs[0].cost, CostOwner::Metered);

        // Anthropic owns its inventory: a model clud has never heard of must
        // still resolve, verbatim, rather than being rejected by a stale list.
        assert_eq!(rungs[1].provider, ModelProvider::Claude);
        assert_eq!(rungs[1].route, ConversationRoute::Claude);
        assert_eq!(rungs[1].wire_id, "claude-opus-4-1");
        assert_eq!(rungs[1].cost, CostOwner::Subscription);
    }

    #[test]
    fn a_provider_without_a_gateway_route_is_rejected_at_parse_time() {
        let error = FailoverLadder::parse("openrouter-claude-sonnet", true).unwrap_err();
        assert_eq!(
            error,
            LadderError::Unroutable {
                spec: "openrouter-claude-sonnet".to_string(),
                provider: "openrouter",
            },
            "a rung that cannot be served must fail the launch, not a turn"
        );
    }

    #[test]
    fn an_empty_spec_is_an_empty_ladder_but_a_stray_comma_is_an_error() {
        assert!(FailoverLadder::parse("", false).unwrap().is_empty());
        assert!(FailoverLadder::parse("   ", false).unwrap().is_empty());
        assert_eq!(
            FailoverLadder::parse("claude-opus-4-1,,codex-terra", false).unwrap_err(),
            LadderError::Empty
        );
    }

    /// The consent gate. A metered rung is real, ordered, and reportable — it
    /// is simply not descended onto until the user says so.
    #[test]
    fn a_metered_rung_is_withheld_until_consent_is_recorded() {
        let now = Instant::now();
        let ledger = RouteLedger::new();
        let spec = "codex-terra,deepseek-v4-pro";

        let guarded = FailoverLadder::parse(spec, false).unwrap();
        assert_eq!(
            guarded
                .next_available(None, &ledger, now)
                .map(|rung| rung.spec.as_str()),
            Some("codex-terra"),
            "a subscription rung needs no consent"
        );
        assert_eq!(
            guarded
                .next_available(Some("codex-terra"), &ledger, now)
                .map(|rung| rung.spec.as_str()),
            None,
            "the metered rung must not be descended onto without consent"
        );
        assert_eq!(
            guarded
                .withheld_for_consent()
                .iter()
                .map(|rung| rung.spec.as_str())
                .collect::<Vec<_>>(),
            vec!["deepseek-v4-pro"]
        );

        let consented = FailoverLadder::parse(spec, true).unwrap();
        assert_eq!(
            consented
                .next_available(Some("codex-terra"), &ledger, now)
                .map(|rung| rung.spec.as_str()),
            Some("deepseek-v4-pro")
        );
        assert!(consented.withheld_for_consent().is_empty());
    }

    /// Descent skips what the ledger already knows cannot serve, rather than
    /// spending a request to rediscover it.
    #[test]
    fn descent_skips_routes_the_ledger_knows_are_cold() {
        let now = Instant::now();
        let ladder =
            FailoverLadder::parse("deepseek-v4-pro,codex-terra,claude-opus-4-1", true).unwrap();
        let mut ledger = RouteLedger::new();
        ledger.record(ConversationRoute::DeepSeek, RouteVerdict::Drained, now);
        ledger.record(
            ConversationRoute::Codex,
            RouteVerdict::Exhausted {
                cooldown: Duration::from_secs(600),
            },
            now,
        );

        assert_eq!(
            ladder
                .next_available(None, &ledger, now)
                .map(|rung| rung.spec.as_str()),
            Some("claude-opus-4-1")
        );
        // ...and once Codex's clock runs out it is preferred again, because it
        // sits higher in the declared order.
        assert_eq!(
            ladder
                .next_available(None, &ledger, now + Duration::from_secs(601))
                .map(|rung| rung.spec.as_str()),
            Some("codex-terra")
        );
    }

    #[test]
    fn descent_resumes_after_the_rung_that_was_just_tried() {
        let now = Instant::now();
        let ledger = RouteLedger::new();
        let ladder =
            FailoverLadder::parse("claude-opus-4-1,claude-sonnet-4-5,codex-terra", true).unwrap();
        assert_eq!(
            ladder
                .next_available(Some("claude-opus-4-1"), &ledger, now)
                .map(|rung| rung.spec.as_str()),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            ladder
                .next_available(Some("codex-terra"), &ledger, now)
                .map(|rung| rung.spec.as_str()),
            None,
            "the ladder must end rather than wrap around and loop forever"
        );
    }

    #[test]
    fn a_rung_label_names_the_cost_owner_and_never_a_credential() {
        let ladder = FailoverLadder::parse("deepseek-v4-flash", true).unwrap();
        assert_eq!(ladder.rungs()[0].label(), "deepseek-v4-flash (metered)");
    }
}
