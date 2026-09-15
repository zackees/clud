//! Choose how toasts render for one PTY session (#1189).
//!
//! | Tier | Where | Restore |
//! |---|---|---|
//! | [`ToastTier::Kitty`] | kitty, Ghostty, WezTerm | free: delete the placement |
//! | [`ToastTier::TextCells`] | everything else, alternate screen only | repaint from the shadow |
//! | [`ToastTier::Fallback`] | no in-grid surface | status line (Claude) or title |
//!
//! Konsole and iTerm2 implement kitty graphics partially; until their z-index,
//! `C=1` and delete-by-placement coverage is verified they get text cells.
//! `CLUD_TOAST_TIER` overrides the decision for manual validation.

use running_process::{CapabilityStatus, EvidenceStrength, GraphicsProtocol, TerminalCapabilities};

pub const TIER_OVERRIDE_ENV: &str = "CLUD_TOAST_TIER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTier {
    Kitty,
    TextCells,
    Fallback,
    /// Toasts disabled for this session.
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierDecision {
    pub tier: ToastTier,
    pub reason: String,
}

/// Decide the tier. `env` reads an environment variable; injected so the
/// decision is testable for every terminal on every platform.
pub fn decide(
    capabilities: Option<&TerminalCapabilities>,
    env: &dyn Fn(&str) -> Option<String>,
    graphics_header_active: bool,
) -> TierDecision {
    if let Some(value) = env(TIER_OVERRIDE_ENV) {
        let tier = match value.trim().to_ascii_lowercase().as_str() {
            "kitty" => Some(ToastTier::Kitty),
            "text" | "cells" => Some(ToastTier::TextCells),
            "fallback" => Some(ToastTier::Fallback),
            "off" | "0" | "false" => Some(ToastTier::Off),
            _ => None,
        };
        if let Some(tier) = tier {
            return decision(tier, format!("{TIER_OVERRIDE_ENV}={value}"));
        }
    }
    if graphics_header_active {
        return decision(
            ToastTier::Fallback,
            "graphics header owns a scroll region; in-grid toasts disabled".into(),
        );
    }
    if let Some(caps) = capabilities {
        if !caps.is_tty {
            return decision(ToastTier::Fallback, "stdout is not a TTY".into());
        }
        if let Some(kitty) = caps
            .graphics
            .protocols
            .iter()
            .find(|c| c.protocol == GraphicsProtocol::Kitty)
        {
            if kitty.status == CapabilityStatus::Supported
                && matches!(
                    kitty.evidence,
                    EvidenceStrength::Probe | EvidenceStrength::StrongHostSignal
                )
            {
                if partial_kitty_terminal(env) {
                    return decision(
                        ToastTier::TextCells,
                        format!(
                            "kitty graphics reported by {} but not yet validated here",
                            kitty.source
                        ),
                    );
                }
                return decision(
                    ToastTier::Kitty,
                    format!("kitty graphics {:?} from {}", kitty.evidence, kitty.source),
                );
            }
        }
    }
    if let Some(reason) = kitty_host_signal(env) {
        return decision(ToastTier::Kitty, reason);
    }
    decision(
        ToastTier::TextCells,
        "no kitty graphics; text cells on the alternate screen".into(),
    )
}

fn decision(tier: ToastTier, reason: String) -> TierDecision {
    TierDecision { tier, reason }
}

/// Environment that identifies a terminal with full kitty graphics support.
fn kitty_host_signal(env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    if env("TERM").is_some_and(|t| t.contains("kitty")) || set("KITTY_WINDOW_ID") {
        return Some("kitty terminal".into());
    }
    if env("TERM").is_some_and(|t| t.contains("ghostty"))
        || env("TERM_PROGRAM").is_some_and(|p| p.eq_ignore_ascii_case("ghostty"))
        || set("GHOSTTY_RESOURCES_DIR")
    {
        return Some("Ghostty terminal".into());
    }
    if env("TERM_PROGRAM").is_some_and(|p| p.eq_ignore_ascii_case("wezterm")) || set("WEZTERM_PANE")
    {
        return Some("WezTerm terminal".into());
    }
    None
}

/// Terminals whose kitty graphics implementation is known to be partial.
fn partial_kitty_terminal(env: &dyn Fn(&str) -> Option<String>) -> bool {
    kitty_host_signal(env).is_none()
        && (env("KONSOLE_VERSION").is_some()
            || env("TERM_PROGRAM").is_some_and(|p| p.eq_ignore_ascii_case("iTerm.app")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use running_process::{GraphicsCapability, TerminalGraphicsCapabilities};
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    fn caps(protocol: GraphicsProtocol, evidence: EvidenceStrength) -> TerminalCapabilities {
        TerminalCapabilities {
            is_tty: true,
            term: None,
            terminal_program: None,
            graphics: TerminalGraphicsCapabilities {
                protocols: vec![GraphicsCapability {
                    protocol,
                    status: CapabilityStatus::Supported,
                    evidence,
                    source: "test".into(),
                    risks: Vec::new(),
                }],
                preferred: Some(protocol),
            },
        }
    }

    #[test]
    fn linux_kitty_environment_selects_the_kitty_tier() {
        let env = env_of(&[("TERM", "xterm-kitty"), ("KITTY_WINDOW_ID", "15")]);
        assert_eq!(decide(None, &env, false).tier, ToastTier::Kitty);
    }

    #[test]
    fn a_kitty_probe_selects_the_kitty_tier() {
        let caps = caps(GraphicsProtocol::Kitty, EvidenceStrength::Probe);
        assert_eq!(
            decide(Some(&caps), &env_of(&[]), false).tier,
            ToastTier::Kitty
        );
    }

    #[test]
    fn ghostty_and_wezterm_select_the_kitty_tier() {
        for pairs in [
            &[("TERM_PROGRAM", "ghostty")][..],
            &[("TERM", "xterm-ghostty")][..],
            &[("TERM_PROGRAM", "WezTerm")][..],
            &[("WEZTERM_PANE", "0")][..],
        ] {
            assert_eq!(
                decide(None, &env_of(pairs), false).tier,
                ToastTier::Kitty,
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn windows_terminal_and_conhost_select_text_cells() {
        let env = env_of(&[("WT_SESSION", "abc"), ("TERM_PROGRAM", "")]);
        assert_eq!(decide(None, &env, false).tier, ToastTier::TextCells);
        assert_eq!(decide(None, &env_of(&[]), false).tier, ToastTier::TextCells);
    }

    #[test]
    fn macos_iterm_and_konsole_stay_on_text_cells_even_when_probed() {
        let caps = caps(GraphicsProtocol::Kitty, EvidenceStrength::Probe);
        let iterm = env_of(&[("TERM_PROGRAM", "iTerm.app")]);
        assert_eq!(
            decide(Some(&caps), &iterm, false).tier,
            ToastTier::TextCells
        );
        let konsole = env_of(&[("KONSOLE_VERSION", "240802")]);
        assert_eq!(
            decide(Some(&caps), &konsole, false).tier,
            ToastTier::TextCells
        );
    }

    #[test]
    fn a_sixel_only_terminal_gets_text_cells_not_sixel() {
        let caps = caps(GraphicsProtocol::Sixel, EvidenceStrength::Probe);
        assert_eq!(
            decide(Some(&caps), &env_of(&[]), false).tier,
            ToastTier::TextCells
        );
    }

    #[test]
    fn the_graphics_header_and_non_tty_force_the_fallback() {
        let kitty = env_of(&[("TERM", "xterm-kitty")]);
        assert_eq!(decide(None, &kitty, true).tier, ToastTier::Fallback);
        let mut caps = caps(GraphicsProtocol::Kitty, EvidenceStrength::Probe);
        caps.is_tty = false;
        assert_eq!(
            decide(Some(&caps), &env_of(&[]), false).tier,
            ToastTier::Fallback
        );
    }

    #[test]
    fn the_override_wins_over_everything() {
        let env = env_of(&[("TERM", "xterm-kitty"), (TIER_OVERRIDE_ENV, "text")]);
        assert_eq!(decide(None, &env, true).tier, ToastTier::TextCells);
        let off = env_of(&[(TIER_OVERRIDE_ENV, "off")]);
        assert_eq!(decide(None, &off, false).tier, ToastTier::Off);
        let junk = env_of(&[("TERM", "xterm-kitty"), (TIER_OVERRIDE_ENV, "nonsense")]);
        assert_eq!(decide(None, &junk, false).tier, ToastTier::Kitty);
    }
}
