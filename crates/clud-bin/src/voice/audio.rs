pub(super) const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Recordings shorter than this are treated as accidental — neither
/// played back nor sent to Whisper. Tuned to ignore key chatter on
/// terminals that bounce SS3-R press/release within a single frame.
pub(super) const MIN_CAPTURE_MS: u128 = 150;
/// Peak-amplitude threshold below which a sample frame is treated as
/// silence. Calibrated for the f32 normalized range [-1, 1].
pub(super) const MAX_SILENCE_PEAK: f32 = 0.01;
/// Issue #13 hold-to-record VAD: once at least `MIN_VAD_AFTER_SPEECH_MS`
/// have elapsed since recording started AND speech has been detected,
/// `VAD_SILENCE_TAIL_MS` of continuous silence triggers an auto-stop.
/// This is the fallback for terminals (ConPTY most notably) that don't
/// emit kitty release events.
pub(super) const MIN_VAD_AFTER_SPEECH_MS: u128 = 800;
pub(super) const VAD_SILENCE_TAIL_MS: u128 = 1_500;
/// Hard safety cap. Whisper transcription quality degrades on long
/// segments anyway, and a stuck recording state would otherwise pin
/// the mic forever.
pub(super) const MAX_CAPTURE_MS: u128 = 30_000;

pub(super) fn downmix_and_resample(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Vec<f32> {
    if samples.is_empty() || channels == 0 || sample_rate == 0 {
        return Vec::new();
    }

    let mono = if channels == 1 {
        samples
    } else {
        samples
            .chunks(channels as usize)
            .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
            .collect::<Vec<_>>()
    };

    if sample_rate == TARGET_SAMPLE_RATE {
        return mono;
    }

    let output_len =
        ((mono.len() as f64) * TARGET_SAMPLE_RATE as f64 / sample_rate as f64).round() as usize;
    if output_len == 0 {
        return Vec::new();
    }

    let mut output = Vec::with_capacity(output_len);
    let last_index = mono.len().saturating_sub(1);
    for i in 0..output_len {
        let source_pos = i as f64 * sample_rate as f64 / TARGET_SAMPLE_RATE as f64;
        let left_index = source_pos.floor() as usize;
        let right_index = left_index.saturating_add(1).min(last_index);
        let frac = (source_pos - left_index as f64) as f32;
        let left = mono[left_index.min(last_index)];
        let right = mono[right_index];
        output.push(left + (right - left) * frac);
    }
    output
}

pub(super) fn is_effectively_silent(samples: &[f32]) -> bool {
    peak_amplitude(samples) < MAX_SILENCE_PEAK
}

/// True when any sample in `slice` rises above `MAX_SILENCE_PEAK` —
/// i.e., the user is actively speaking in this window. The VAD
/// auto-stop uses this on a rolling-tail basis.
pub(super) fn has_speech_peak(slice: &[f32]) -> bool {
    peak_amplitude(slice) >= MAX_SILENCE_PEAK
}

/// Peak absolute amplitude across `samples`. Returns 0.0 for an empty
/// slice. Reused by [`is_effectively_silent`] and [`has_speech_peak`]
/// so both definitions track the same threshold.
fn peak_amplitude(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0f32, |max, sample| max.max(sample.abs()))
}
