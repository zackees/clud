use std::sync::{Arc, Mutex};

use super::audio::{
    downmix_and_resample, has_speech_peak, is_effectively_silent, MAX_CAPTURE_MS, MAX_SILENCE_PEAK,
    MIN_CAPTURE_MS, MIN_VAD_AFTER_SPEECH_MS, VAD_SILENCE_TAIL_MS,
};
use crate::platform::voice::MicCapture;

pub(super) struct ActiveRecording {
    started_at: std::time::Instant,
    sample_rate: u32,
    channels: u16,
    samples: Arc<Mutex<Vec<f32>>>,
    /// `None` when this recording was constructed via
    /// [`ActiveRecording::synthetic`] for tests or for the
    /// `CLUD_VOICE_TEST_TRANSCRIPT` bypass — no mic is owned, so
    /// there is nothing to stop or drop. `Some` for real captures:
    /// a cpal stream on Windows and macOS, an `arecord` helper on Linux.
    capture: Option<MicCapture>,
    /// Number of source-sample frames the recording had collected the
    /// last time the VAD checker ran. The tail-silence detector
    /// inspects only the new slice since the previous check.
    last_vad_offset: usize,
    /// Wall-clock instant when speech was last observed (peak above
    /// `MAX_SILENCE_PEAK`). `None` until the first non-silent frame.
    last_speech_at: Option<std::time::Instant>,
}

impl ActiveRecording {
    pub(super) fn start() -> Result<Self, String> {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let (capture, sample_rate, channels) = MicCapture::start(Arc::clone(&samples))?;

        Ok(Self {
            started_at: std::time::Instant::now(),
            sample_rate,
            channels,
            samples,
            capture: Some(capture),
            last_vad_offset: 0,
            last_speech_at: None,
        })
    }

    /// Construct a recording with no real microphone stream. Used by
    /// the `CLUD_VOICE_TEST_TRANSCRIPT` bypass (so the integration
    /// test on CI runners with no audio device still exercises
    /// observer → VoiceMode → write_impl) and by the Rust state-
    /// machine unit tests (so they can populate `mode.recording`
    /// without standing up a cpal stream — that was crashing
    /// `cargo test` with STATUS_ACCESS_VIOLATION on Windows runners
    /// whose default WASAPI device returned a stream that segfaulted
    /// on drop without a prior `.play()`).
    ///
    /// Pre-populates the sample buffer with non-silent dummy audio
    /// so `finish()` returns a non-empty vec and `stop_recording`
    /// queues a transcription — the worker then short-circuits to
    /// `config.test_transcript` and writes the result back into the
    /// PTY.
    pub(super) fn synthetic() -> Self {
        // Backdate `started_at` past `MIN_CAPTURE_MS` so `finish()`
        // accepts the dummy capture as long enough to keep.
        let backdated = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(
                (MIN_CAPTURE_MS as u64) + 50,
            ))
            .unwrap_or_else(std::time::Instant::now);
        let sample_rate: u32 = 16_000;
        // ~200 ms of a constant non-silent tone — enough samples to
        // survive downmix+resample and clear `is_effectively_silent`.
        let count = (sample_rate as usize) / 5;
        let dummy = vec![MAX_SILENCE_PEAK * 4.0; count];
        Self {
            started_at: backdated,
            sample_rate,
            channels: 1,
            samples: Arc::new(Mutex::new(dummy)),
            capture: None,
            last_vad_offset: 0,
            last_speech_at: None,
        }
    }

    /// Decide whether the recording should auto-stop based on:
    /// 1. Hard cap reached (`MAX_CAPTURE_MS`), OR
    /// 2. The user has spoken at least once AND been silent for
    ///    `VAD_SILENCE_TAIL_MS` since their last speech frame.
    ///
    /// Called every PTY-pump tick. Cheap: scans only the new slice
    /// of samples since the last call and tracks state on `self`.
    pub(super) fn should_auto_stop(&mut self) -> bool {
        let now = std::time::Instant::now();
        let elapsed_ms = now.duration_since(self.started_at).as_millis();

        if elapsed_ms >= MAX_CAPTURE_MS {
            return true;
        }

        // Snapshot the current sample count, then walk the new slice
        // looking for a peak above the silence threshold. Holding the
        // lock for the full scan is fine — the audio thread only
        // appends, and the bounded slice keeps the hold short.
        let new_offset = match self.samples.lock() {
            Ok(buffer) => {
                let new_offset = buffer.len();
                if new_offset > self.last_vad_offset {
                    let new_slice = &buffer[self.last_vad_offset..new_offset];
                    if has_speech_peak(new_slice) {
                        self.last_speech_at = Some(now);
                    }
                }
                new_offset
            }
            Err(_) => return false,
        };
        self.last_vad_offset = new_offset;

        // Only auto-stop on silence if the user has actually spoken —
        // otherwise an enabled-but-quiet mic would stop instantly.
        // Also require a minimum recording duration so a quick blip
        // of speech doesn't immediately terminate.
        let Some(last_speech) = self.last_speech_at else {
            return false;
        };
        if elapsed_ms < MIN_VAD_AFTER_SPEECH_MS {
            return false;
        }
        let silence_ms = now.duration_since(last_speech).as_millis();
        silence_ms >= VAD_SILENCE_TAIL_MS
    }

    pub(super) fn finish(mut self) -> Result<Vec<f32>, String> {
        let elapsed_ms = self.started_at.elapsed().as_millis();
        // Stop the capture backend first so callbacks/readers can't
        // append more frames while we drain. Synthetic recordings have
        // no backend, so this is a no-op.
        self.stop_capture_backend()?;

        let samples = match Arc::try_unwrap(self.samples) {
            Ok(samples) => samples
                .into_inner()
                .map_err(|_| "microphone sample buffer lock poisoned".to_string())?,
            Err(shared) => shared
                .lock()
                .map_err(|_| "microphone sample buffer lock poisoned".to_string())?
                .clone(),
        };

        if elapsed_ms < MIN_CAPTURE_MS {
            return Ok(Vec::new());
        }

        let resampled = downmix_and_resample(samples, self.channels, self.sample_rate);
        if is_effectively_silent(&resampled) {
            return Ok(Vec::new());
        }
        Ok(resampled)
    }

    fn stop_capture_backend(&mut self) -> Result<(), String> {
        if let Some(capture) = self.capture.take() {
            capture.stop()?;
        }
        Ok(())
    }
}
