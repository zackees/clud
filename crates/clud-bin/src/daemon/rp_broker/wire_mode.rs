use super::RUNNING_PROCESS_DISABLE_ENV;

/// Wire mode selector at the clud daemon boundary
/// (zackees/running-process#436, consumer-adoption-clud.md step 1).
///
/// Both arms decode into the SAME internal [`DaemonRequest`] /
/// [`DaemonResponse`] model via `wire_prost`:
/// - [`WireMode::JsonLegacy`]: clud's legacy JSON line wire over the
///   direct daemon endpoint (TCP). Selected by `RUNNING_PROCESS_DISABLE=1`.
/// - [`WireMode::ProstV1`]: running-process v1 `Frame` lane (payload
///   protocol `0x7C4C`) reached through [`BrokerSession::adopt`] /
///   Hello-skip, carrying clud prost payloads. The release default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum WireMode {
    /// Legacy JSON over the direct daemon endpoint (escape hatch).
    JsonLegacy,
    /// running-process v1 prost frame lane via the broker session.
    ProstV1,
}

impl WireMode {
    /// Select the active wire mode from the environment. The canonical
    /// escape hatch `RUNNING_PROCESS_DISABLE=1` forces [`Self::JsonLegacy`]
    /// + the direct endpoint; everything else uses [`Self::ProstV1`].
    pub(in crate::daemon) fn select() -> Self {
        if running_process_disabled() {
            Self::JsonLegacy
        } else {
            Self::ProstV1
        }
    }

    /// Stable identifier reported by the daemon diagnostics CLI.
    pub(in crate::daemon) fn as_str(self) -> &'static str {
        match self {
            Self::JsonLegacy => "json-legacy",
            Self::ProstV1 => "prost-v1",
        }
    }
}

pub(super) fn running_process_disabled() -> bool {
    std::env::var(RUNNING_PROCESS_DISABLE_ENV)
        .map(|value| value == "1")
        .unwrap_or(false)
}
