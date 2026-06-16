use running_process::broker::adopt::AdoptError;
use running_process::broker::client::RefusalKind;

/// Classify a [`BrokerSession::adopt`] failure for one diagnostic line
/// before falling back to the TCP wire (#436 step 6, typed `Refused`).
///
/// `BrokerDisabled` is the escape hatch, not a failure, so it stays
/// silent. A broker refusal is reported through [`RefusalKind`] rather
/// than string-matched; everything else (unreachable broker, dial/IO
/// error) is a plain miss and also stays quiet — the TCP fallback covers
/// it on every launch.
pub(super) fn log_adopt_miss(err: &AdoptError) {
    if let AdoptError::Connect(connect) = err {
        match connect.refusal_kind() {
            Some(RefusalKind::VersionUnsupported) => eprintln!(
                "[clud] note: broker refused clud (version unsupported); upgrade running-process. Falling back to TCP."
            ),
            Some(RefusalKind::VersionBlocked) => eprintln!(
                "[clud] note: broker refused clud (daemon version blocked). Falling back to TCP."
            ),
            Some(RefusalKind::ServiceUnknown) => eprintln!(
                "[clud] note: broker has no clud.servicedef (service unknown). Falling back to TCP."
            ),
            Some(RefusalKind::RateLimited) => eprintln!(
                "[clud] note: broker rate-limited clud. Falling back to TCP."
            ),
            Some(RefusalKind::ShuttingDown) => eprintln!(
                "[clud] note: broker is shutting down. Falling back to TCP."
            ),
            Some(RefusalKind::Other(code)) => {
                eprintln!("[clud] note: broker refused clud (code {code:?}). Falling back to TCP.")
            }
            // None => not a refusal (broker unreachable / dial error);
            // the TCP fallback handles it on every launch, so stay quiet.
            None => {}
        }
    }
}
