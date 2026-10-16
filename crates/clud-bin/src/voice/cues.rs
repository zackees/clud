use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub(super) enum CueTone {
    Start,
    Stop,
}

/// Play the cue for `tone`. Windows and macOS play a short sine tone
/// (falling back to the terminal bell); Linux rings the bell. See
/// `crate::platform::voice::play_cue`.
pub(super) fn play_cue(tone: CueTone) {
    let freq_hz = match tone {
        CueTone::Start => 880.0,
        CueTone::Stop => 660.0,
    };
    let duration = match tone {
        CueTone::Start => Duration::from_millis(90),
        CueTone::Stop => Duration::from_millis(120),
    };
    crate::platform::voice::play_cue(freq_hz, duration);
}
