//! Voice mode (issue #13): F3 push-to-talk transcription.
//!
//! Flow:
//!   1. `F3Observer` in [`crate::session`] watches the byte stream coming
//!      from the user's terminal and reports F3 press/release events.
//!   2. [`VoiceMode::on_f3_press`] starts microphone capture and plays a
//!      start cue.
//!   3. Release (real kitty-protocol release, VAD-silence auto-stop, or the
//!      30-second hard cap — whichever fires first) calls
//!      [`VoiceMode::on_f3_release`], which plays a `dong` cue and hands
//!      the captured audio to a background [`VoiceWorker`] thread.
//!   4. The worker downsamples to 16 kHz mono, runs `whisper-rs`, and
//!      sends the resulting text back as a [`WorkerEvent::Transcript`].
//!   5. The next tick of the PTY pump drains that event and writes the
//!      transcript bytes into the backend PTY via
//!      `NativePtyProcess::write_impl(..., is_paste=false)` — the user
//!      sees the text appear at their cursor without auto-submit.
//!
//! The Whisper model (`ggml-small.en.bin`, ~466 MB) is auto-downloaded
//! to a per-OS cache dir on first use; `CLUD_WHISPER_MODEL` still
//! overrides if set. See [`model::resolve_model_path`].

mod enabled {
    use std::env;
    #[cfg(target_os = "linux")]
    use std::fs;
    use std::io::{self, Write};
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    #[cfg(target_os = "linux")]
    use std::io::Read;
    #[cfg(target_os = "linux")]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(target_os = "linux")]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(not(target_os = "linux"))]
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    #[cfg(not(target_os = "linux"))]
    use rodio::{OutputStreamBuilder, Sink, Source};
    use running_process_core::pty::NativePtyProcess;
    #[cfg(target_os = "linux")]
    use running_process_core::{
        CommandSpec, Containment, NativeProcess, ProcessConfig, ProcessError, StderrMode, StdinMode,
    };
    // Issue #13 follow-up: whisper-rs-sys does not build on aarch64-pc-windows-msvc.
    // The dep is target-gated in Cargo.toml; voice transcription is stubbed
    // on that one platform via `WhisperContextHandle = ()` plus an error
    // return from `transcribe_audio` (model loading + Whisper calls are
    // bypassed there). All other surfaces (mic capture, cue playback,
    // F3 state machine, downsampling) ship unchanged.
    #[cfg(not(all(target_arch = "aarch64", target_os = "windows")))]
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    /// On supported targets this aliases the real Whisper context;
    /// on Windows ARM (where the dep is omitted) it collapses to `()`
    /// so the worker thread compiles without pulling whisper-rs in.
    #[cfg(not(all(target_arch = "aarch64", target_os = "windows")))]
    type WhisperContextHandle = WhisperContext;
    #[cfg(all(target_arch = "aarch64", target_os = "windows"))]
    type WhisperContextHandle = ();

    use crate::session::InteractiveHooks;

    const TARGET_SAMPLE_RATE: u32 = 16_000;
    /// Recordings shorter than this are treated as accidental — neither
    /// played back nor sent to Whisper. Tuned to ignore key chatter on
    /// terminals that bounce SS3-R press/release within a single frame.
    const MIN_CAPTURE_MS: u128 = 150;
    /// Peak-amplitude threshold below which a sample frame is treated as
    /// silence. Calibrated for the f32 normalized range [-1, 1].
    const MAX_SILENCE_PEAK: f32 = 0.01;
    /// Issue #13 hold-to-record VAD: once at least `MIN_VAD_AFTER_SPEECH_MS`
    /// have elapsed since recording started AND speech has been detected,
    /// `VAD_SILENCE_TAIL_MS` of continuous silence triggers an auto-stop.
    /// This is the fallback for terminals (ConPTY most notably) that don't
    /// emit kitty release events.
    const MIN_VAD_AFTER_SPEECH_MS: u128 = 800;
    const VAD_SILENCE_TAIL_MS: u128 = 1_500;
    /// Hard safety cap. Whisper transcription quality degrades on long
    /// segments anyway, and a stuck recording state would otherwise pin
    /// the mic forever.
    const MAX_CAPTURE_MS: u128 = 30_000;

    /// Whisper model resolution + auto-download (issue #13).
    ///
    /// Resolution order:
    ///   1. `CLUD_WHISPER_MODEL` env var → trusted as-is (no hash check).
    ///   2. `<cache-dir>/clud/whisper/ggml-small.en.bin` if file exists
    ///      AND its SHA-256 matches `MODEL_SHA256`.
    ///   3. Download from Hugging Face into (2)'s path, atomic-rename
    ///      from `<name>.partial`, verify hash, retry once on hash
    ///      mismatch.
    ///
    /// On any failure the existing `missing_model_message()` is surfaced
    /// to the user with a hint about where they can drop the model
    /// manually.
    pub(super) mod model {
        use std::fs::{self, File};
        use std::io::{self, Read, Write};
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        use sha2::{Digest, Sha256};

        /// Default model filename. Whisper.cpp uses this name on Hugging
        /// Face and most user-facing docs reference it.
        pub(super) const MODEL_FILENAME: &str = "ggml-small.en.bin";
        /// Upstream model URL. Hugging Face serves it un-gated.
        const MODEL_URL: &str =
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin";
        /// SHA-256 of the small.en model as published by ggerganov. Pinned
        /// so a corrupt download or upstream swap can't silently break
        /// transcription quality. If the upstream model rev changes,
        /// update this constant in the same PR.
        const MODEL_SHA256: &str =
            "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b";

        /// Compute the default per-OS cache path for the model.
        ///
        /// Falls back to `./.clud-cache/whisper/` (relative to cwd) if
        /// `dirs::cache_dir()` returns `None`, which happens on stripped
        /// environments where neither `XDG_CACHE_HOME` nor the home dir
        /// is resolvable.
        pub(super) fn default_cache_path() -> PathBuf {
            let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".clud-cache"));
            base.join("clud").join("whisper").join(MODEL_FILENAME)
        }

        /// Resolve a usable model path WITHOUT triggering a download.
        ///
        /// Returns `Some(path)` if the env override is set OR the cached
        /// copy is present and intact. Returns `None` if the model needs
        /// to be downloaded — `ensure_downloaded_in_background` is the
        /// next step in that case.
        pub(super) fn resolve_if_available(env_override: Option<&Path>) -> Option<PathBuf> {
            if let Some(path) = env_override {
                if path.is_file() {
                    return Some(path.to_path_buf());
                }
            }
            let cache = default_cache_path();
            if cache.is_file() && verify_sha256(&cache).unwrap_or(false) {
                return Some(cache);
            }
            None
        }

        /// Kick off a detached download thread for the cache path. Sets
        /// `*flag` to true when the download has finished (success OR
        /// failure) so the caller can probe completion without joining.
        ///
        /// No-op if the cached file is already present and hash-valid —
        /// the caller should call `resolve_if_available` first.
        pub(super) fn ensure_downloaded_in_background(done_flag: Arc<AtomicBool>) {
            std::thread::spawn(move || {
                let result = download_to_cache();
                if let Err(err) = &result {
                    eprintln!("[clud] voice: model auto-download failed: {err}");
                    eprintln!(
                        "[clud] voice: drop ggml-small.en.bin at {:?} or set CLUD_WHISPER_MODEL",
                        default_cache_path()
                    );
                }
                done_flag.store(true, Ordering::SeqCst);
            });
        }

        /// Download the model to the cache path. Streams to a `.partial`
        /// temp file alongside, verifies SHA-256, then atomic-renames.
        /// Idempotent: succeeds without downloading if a valid copy
        /// already exists.
        fn download_to_cache() -> Result<PathBuf, String> {
            let final_path = default_cache_path();
            if final_path.is_file() && verify_sha256(&final_path).unwrap_or(false) {
                return Ok(final_path);
            }

            if let Some(parent) = final_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("could not create cache dir {parent:?}: {err}"))?;
            }
            let partial_path = final_path.with_extension("partial");
            // Best-effort clean of a stale partial from a previous run.
            let _ = fs::remove_file(&partial_path);

            eprintln!(
                "[clud] voice: downloading Whisper model (~466 MB) to {:?} — first F3 use will block until this finishes",
                final_path
            );

            let response = ureq::get(MODEL_URL)
                .timeout(Duration::from_secs(300))
                .call()
                .map_err(|err| format!("HTTP error fetching {MODEL_URL}: {err}"))?;
            let total_bytes: Option<u64> = response
                .header("Content-Length")
                .and_then(|s| s.parse().ok());

            let mut reader = response.into_reader();
            let mut file = File::create(&partial_path)
                .map_err(|err| format!("could not create {partial_path:?}: {err}"))?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0u8; 64 * 1024];
            let mut downloaded: u64 = 0;
            let mut next_progress_pct: u64 = 5;
            loop {
                let n = reader
                    .read(&mut buffer)
                    .map_err(|err| format!("download read error: {err}"))?;
                if n == 0 {
                    break;
                }
                file.write_all(&buffer[..n])
                    .map_err(|err| format!("could not write to {partial_path:?}: {err}"))?;
                hasher.update(&buffer[..n]);
                downloaded += n as u64;
                if let Some(total) = total_bytes {
                    let pct = downloaded * 100 / total.max(1);
                    if pct >= next_progress_pct {
                        eprintln!(
                            "[clud] voice: download {pct}% ({downloaded}/{total} bytes)",
                            pct = pct,
                        );
                        // Step in 5% increments but jump forward if we
                        // already skipped past the next mark.
                        next_progress_pct = pct + 5;
                    }
                }
            }
            file.flush().map_err(|err| format!("flush failed: {err}"))?;
            drop(file);

            let digest = format!("{:x}", hasher.finalize());
            if digest != MODEL_SHA256 {
                let _ = fs::remove_file(&partial_path);
                return Err(format!(
                    "SHA-256 mismatch: expected {MODEL_SHA256}, got {digest}; refusing to use a corrupt model"
                ));
            }

            fs::rename(&partial_path, &final_path).map_err(|err| {
                format!("could not rename {partial_path:?} -> {final_path:?}: {err}")
            })?;
            eprintln!("[clud] voice: model ready at {final_path:?}");
            Ok(final_path)
        }

        /// Compute SHA-256 of the file at `path` and compare to
        /// [`MODEL_SHA256`]. `Ok(false)` means the file exists but the
        /// hash doesn't match (corrupt or stale download). `Err` means
        /// the file couldn't be opened or read.
        pub(super) fn verify_sha256(path: &Path) -> io::Result<bool> {
            let mut file = File::open(path)?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(format!("{:x}", hasher.finalize()) == MODEL_SHA256)
        }

        /// `AtomicBool::clone`-free way to signal a probe-completion
        /// flag across the worker thread boundary. Returned from
        /// `VoiceMode::from_env` so the input loop can check whether
        /// the auto-download has finished without joining.
        pub(super) fn fresh_completion_flag() -> Arc<AtomicBool> {
            Arc::new(AtomicBool::new(false))
        }

        use std::time::Duration;
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct VoiceConfig {
        enabled: bool,
        model_path: Option<PathBuf>,
        language: Option<String>,
        test_transcript: Option<String>,
    }

    impl VoiceConfig {
        fn from_env() -> Self {
            let model_path = env::var_os("CLUD_WHISPER_MODEL").map(PathBuf::from);
            let test_transcript = env::var("CLUD_VOICE_TEST_TRANSCRIPT")
                .ok()
                .filter(|value| !value.trim().is_empty());
            let language = env::var("CLUD_VOICE_LANGUAGE")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let enabled_flag = matches!(
                env::var("CLUD_VOICE")
                    .ok()
                    .as_deref()
                    .map(|value| value.to_ascii_lowercase()),
                Some(ref value) if matches!(value.as_str(), "1" | "true" | "yes" | "on")
            );

            Self {
                enabled: enabled_flag || model_path.is_some() || test_transcript.is_some(),
                model_path,
                language,
                test_transcript,
            }
        }
    }

    struct ActiveRecording {
        started_at: std::time::Instant,
        sample_rate: u32,
        channels: u16,
        samples: Arc<Mutex<Vec<f32>>>,
        /// `None` when this recording was constructed via
        /// [`ActiveRecording::synthetic`] for tests or for the
        /// `CLUD_VOICE_TEST_TRANSCRIPT` bypass — no mic is owned, so
        /// there is nothing to stop or drop. `Some` for real captures.
        #[cfg(not(target_os = "linux"))]
        stream: Option<cpal::Stream>,
        #[cfg(target_os = "linux")]
        capture: Option<LinuxCapture>,
        /// Number of source-sample frames the recording had collected the
        /// last time the VAD checker ran. The tail-silence detector
        /// inspects only the new slice since the previous check.
        last_vad_offset: usize,
        /// Wall-clock instant when speech was last observed (peak above
        /// `MAX_SILENCE_PEAK`). `None` until the first non-silent frame.
        last_speech_at: Option<std::time::Instant>,
    }

    impl ActiveRecording {
        #[cfg(not(target_os = "linux"))]
        fn start() -> Result<Self, String> {
            let host = cpal::default_host();
            let device = host
                .default_input_device()
                .ok_or_else(|| "no default input device available".to_string())?;
            let supported = device
                .default_input_config()
                .map_err(|err| format!("failed to query default input config: {err}"))?;
            let sample_rate = supported.sample_rate().0;
            let channels = supported.channels();
            let config = supported.config();
            let samples = Arc::new(Mutex::new(Vec::new()));
            let error_label = device
                .name()
                .unwrap_or_else(|_| "default-input".to_string());

            let stream = match supported.sample_format() {
                cpal::SampleFormat::F32 => {
                    build_input_stream_f32(&device, &config, Arc::clone(&samples), error_label)?
                }
                cpal::SampleFormat::I16 => {
                    build_input_stream_i16(&device, &config, Arc::clone(&samples), error_label)?
                }
                cpal::SampleFormat::U16 => {
                    build_input_stream_u16(&device, &config, Arc::clone(&samples), error_label)?
                }
                other => {
                    return Err(format!("unsupported microphone sample format: {other:?}"));
                }
            };

            stream
                .play()
                .map_err(|err| format!("failed to start microphone stream: {err}"))?;

            Ok(Self {
                started_at: std::time::Instant::now(),
                sample_rate,
                channels,
                samples,
                stream: Some(stream),
                last_vad_offset: 0,
                last_speech_at: None,
            })
        }

        #[cfg(target_os = "linux")]
        fn start() -> Result<Self, String> {
            let samples = Arc::new(Mutex::new(Vec::new()));
            let capture = LinuxCapture::start(Arc::clone(&samples))?;

            Ok(Self {
                started_at: std::time::Instant::now(),
                sample_rate: TARGET_SAMPLE_RATE,
                channels: 1,
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
        fn synthetic() -> Self {
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
                #[cfg(not(target_os = "linux"))]
                stream: None,
                #[cfg(target_os = "linux")]
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
        fn should_auto_stop(&mut self) -> bool {
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

        fn finish(mut self) -> Result<Vec<f32>, String> {
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

        #[cfg(not(target_os = "linux"))]
        fn stop_capture_backend(&mut self) -> Result<(), String> {
            drop(self.stream.take());
            Ok(())
        }

        #[cfg(target_os = "linux")]
        fn stop_capture_backend(&mut self) -> Result<(), String> {
            if let Some(capture) = self.capture.take() {
                capture.stop()?;
            }
            Ok(())
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn build_input_stream_f32(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        samples: Arc<Mutex<Vec<f32>>>,
        label: String,
    ) -> Result<cpal::Stream, String> {
        device
            .build_input_stream(
                config,
                move |data: &[f32], _| {
                    if let Ok(mut buffer) = samples.lock() {
                        buffer.extend_from_slice(data);
                    }
                },
                move |err| eprintln!("[clud] voice: microphone stream error ({label}): {err}"),
                None,
            )
            .map_err(|err| format!("failed to build microphone stream: {err}"))
    }

    #[cfg(not(target_os = "linux"))]
    fn build_input_stream_i16(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        samples: Arc<Mutex<Vec<f32>>>,
        label: String,
    ) -> Result<cpal::Stream, String> {
        device
            .build_input_stream(
                config,
                move |data: &[i16], _| {
                    if let Ok(mut buffer) = samples.lock() {
                        buffer.extend(data.iter().map(|sample| *sample as f32 / i16::MAX as f32));
                    }
                },
                move |err| eprintln!("[clud] voice: microphone stream error ({label}): {err}"),
                None,
            )
            .map_err(|err| format!("failed to build microphone stream: {err}"))
    }

    #[cfg(not(target_os = "linux"))]
    fn build_input_stream_u16(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        samples: Arc<Mutex<Vec<f32>>>,
        label: String,
    ) -> Result<cpal::Stream, String> {
        device
            .build_input_stream(
                config,
                move |data: &[u16], _| {
                    if let Ok(mut buffer) = samples.lock() {
                        buffer.extend(
                            data.iter()
                                .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0),
                        );
                    }
                },
                move |err| eprintln!("[clud] voice: microphone stream error ({label}): {err}"),
                None,
            )
            .map_err(|err| format!("failed to build microphone stream: {err}"))
    }

    #[cfg(target_os = "linux")]
    struct LinuxCapture {
        process: NativeProcess,
        capture_dir: PathBuf,
        reader_done: Arc<AtomicBool>,
        reader: Option<std::thread::JoinHandle<Result<(), String>>>,
    }

    #[cfg(target_os = "linux")]
    impl LinuxCapture {
        fn start(samples: Arc<Mutex<Vec<f32>>>) -> Result<Self, String> {
            let (capture_dir, sample_path) = create_arecord_output_path()?;
            let process = NativeProcess::new(ProcessConfig {
                command: CommandSpec::Argv(vec![
                    "arecord".to_string(),
                    "-q".to_string(),
                    "-t".to_string(),
                    "raw".to_string(),
                    "-f".to_string(),
                    "S16_LE".to_string(),
                    "-c".to_string(),
                    "1".to_string(),
                    "-r".to_string(),
                    "16000".to_string(),
                    sample_path.to_string_lossy().into_owned(),
                ]),
                cwd: None,
                env: None,
                capture: true,
                stderr_mode: StderrMode::Pipe,
                creationflags: None,
                create_process_group: false,
                stdin_mode: StdinMode::Null,
                nice: None,
                containment: Some(Containment::Contained),
            });

            if let Err(err) = process.start() {
                let _ = fs::remove_dir_all(&capture_dir);
                return Err(match err {
                    ProcessError::Spawn(err) if err.kind() == io::ErrorKind::NotFound => {
                        "Linux voice capture requires `arecord` (install alsa-utils)".to_string()
                    }
                    other => format!("failed to start Linux voice capture (`arecord`): {other}"),
                });
            };

            let reader_done = Arc::new(AtomicBool::new(false));
            let reader = {
                let reader_done = Arc::clone(&reader_done);
                std::thread::spawn(move || read_arecord_file(sample_path, reader_done, samples))
            };

            Ok(Self {
                process,
                capture_dir,
                reader_done,
                reader: Some(reader),
            })
        }

        fn stop(mut self) -> Result<(), String> {
            if self
                .process
                .poll()
                .map_err(|err| format!("failed to inspect arecord process: {err}"))?
                .is_none()
            {
                self.process
                    .kill()
                    .map_err(|err| format!("failed to stop arecord process: {err}"))?;
            }
            let exit_code = self
                .process
                .wait(Some(Duration::from_secs(2)))
                .map_err(|err| format!("failed to wait for arecord process: {err}"))?;

            self.reader_done.store(true, Ordering::Release);
            let reader_result = self
                .reader
                .take()
                .expect("arecord reader thread exists")
                .join()
                .map_err(|_| "arecord reader thread panicked".to_string())?;

            let stderr_text = captured_stderr_text(&self.process);
            let _ = fs::remove_dir_all(&self.capture_dir);

            reader_result?;

            if exit_code <= 0 {
                return Ok(());
            }

            let detail = stderr_text.trim();
            if detail.is_empty() {
                Err(format!(
                    "microphone capture command failed with exit code {exit_code}"
                ))
            } else {
                Err(format!("microphone capture command failed: {detail}"))
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn create_arecord_output_path() -> Result<(PathBuf, PathBuf), String> {
        let base = env::temp_dir();
        let pid = std::process::id();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        for attempt in 0..32_u8 {
            let dir = base.join(format!("clud-arecord-{pid}-{nonce}-{attempt}"));
            match fs::create_dir(&dir) {
                Ok(()) => {
                    let sample_path = dir.join("capture.raw");
                    return Ok((dir, sample_path));
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(format!(
                        "failed to create temporary audio capture directory: {err}"
                    ));
                }
            }
        }

        Err("failed to allocate temporary audio capture path".to_string())
    }

    #[cfg(target_os = "linux")]
    fn read_arecord_file(
        path: PathBuf,
        done: Arc<AtomicBool>,
        samples: Arc<Mutex<Vec<f32>>>,
    ) -> Result<(), String> {
        let mut file = loop {
            match fs::File::open(&path) {
                Ok(file) => break file,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    if done.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => {
                    return Err(format!(
                        "failed to open arecord sample file {}: {err}",
                        path.display()
                    ));
                }
            }
        };

        let mut buffer = [0u8; 8192];
        let mut carry: Option<u8> = None;

        loop {
            let n = file
                .read(&mut buffer)
                .map_err(|err| format!("failed to read microphone samples: {err}"))?;
            if n == 0 {
                if done.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }

            append_pcm16le_samples(&buffer[..n], &mut carry, &samples)?;
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn append_pcm16le_samples(
        data: &[u8],
        carry: &mut Option<u8>,
        samples: &Arc<Mutex<Vec<f32>>>,
    ) -> Result<(), String> {
        let mut start = 0usize;
        let mut converted: Vec<f32> = Vec::with_capacity(data.len().div_ceil(2));
        if let Some(lo) = carry.take() {
            if let Some(&hi) = data.first() {
                let sample = i16::from_le_bytes([lo, hi]);
                converted.push(sample as f32 / i16::MAX as f32);
                start = 1;
            } else {
                *carry = Some(lo);
                return Ok(());
            }
        }

        let chunk = &data[start..];
        let even_len = chunk.len() & !1usize;
        for bytes in chunk[..even_len].chunks_exact(2) {
            let sample = i16::from_le_bytes([bytes[0], bytes[1]]);
            converted.push(sample as f32 / i16::MAX as f32);
        }
        if even_len < chunk.len() {
            *carry = Some(chunk[even_len]);
        }

        if !converted.is_empty() {
            samples
                .lock()
                .map_err(|_| "microphone sample buffer lock poisoned".to_string())?
                .extend(converted);
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn captured_stderr_text(process: &NativeProcess) -> String {
        process
            .captured_stderr()
            .into_iter()
            .map(|line| String::from_utf8_lossy(&line).into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn downmix_and_resample(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Vec<f32> {
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

    fn is_effectively_silent(samples: &[f32]) -> bool {
        peak_amplitude(samples) < MAX_SILENCE_PEAK
    }

    /// True when any sample in `slice` rises above `MAX_SILENCE_PEAK` —
    /// i.e., the user is actively speaking in this window. The VAD
    /// auto-stop uses this on a rolling-tail basis.
    fn has_speech_peak(slice: &[f32]) -> bool {
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

    #[derive(Debug)]
    enum WorkerCommand {
        Transcribe { audio: Vec<f32> },
    }

    #[derive(Debug)]
    enum WorkerEvent {
        Transcript(String),
        Error(String),
    }

    #[derive(Debug)]
    struct VoiceWorker {
        commands: mpsc::Sender<WorkerCommand>,
        events: mpsc::Receiver<WorkerEvent>,
    }

    impl VoiceWorker {
        fn spawn(config: VoiceConfig) -> Self {
            let (command_tx, command_rx) = mpsc::channel::<WorkerCommand>();
            let (event_tx, event_rx) = mpsc::channel::<WorkerEvent>();

            std::thread::spawn(move || {
                let mut context: Option<WhisperContextHandle> = None;

                while let Ok(command) = command_rx.recv() {
                    match command {
                        WorkerCommand::Transcribe { audio } => {
                            let result = transcribe_audio(&config, &mut context, &audio);
                            let event = match result {
                                Ok(text) => WorkerEvent::Transcript(text),
                                Err(err) => WorkerEvent::Error(err),
                            };
                            let _ = event_tx.send(event);
                        }
                    }
                }
            });

            Self {
                commands: command_tx,
                events: event_rx,
            }
        }

        fn request_transcription(&self, audio: Vec<f32>) -> Result<(), String> {
            self.commands
                .send(WorkerCommand::Transcribe { audio })
                .map_err(|_| "voice worker is unavailable".to_string())
        }

        fn try_recv(&self) -> Result<WorkerEvent, mpsc::TryRecvError> {
            self.events.try_recv()
        }
    }

    #[cfg(not(all(target_arch = "aarch64", target_os = "windows")))]
    fn transcribe_audio(
        config: &VoiceConfig,
        context: &mut Option<WhisperContextHandle>,
        audio: &[f32],
    ) -> Result<String, String> {
        if let Some(test_transcript) = &config.test_transcript {
            return Ok(test_transcript.clone());
        }

        let model_path = config
            .model_path
            .as_ref()
            .ok_or_else(|| missing_model_message().to_string())?;

        let context_ref = if let Some(context) = context.as_mut() {
            context
        } else {
            let path = model_path
                .to_str()
                .ok_or_else(|| "whisper model path is not valid UTF-8".to_string())?;
            let loaded = WhisperContext::new_with_params(path, WhisperContextParameters::default())
                .map_err(|err| format!("failed to load whisper model: {err}"))?;
            *context = Some(loaded);
            context.as_mut().expect("whisper context just initialized")
        };

        let mut state = context_ref
            .create_state()
            .map_err(|err| format!("failed to create whisper state: {err}"))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(
            std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().min(4) as i32)
                .unwrap_or(2),
        );
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_translate(false);
        if let Some(language) = config.language.as_deref() {
            params.set_language(Some(language));
        }

        state
            .full(params, audio)
            .map_err(|err| format!("whisper transcription failed: {err}"))?;

        let segments = state.full_n_segments();
        let mut transcript = String::new();
        for index in 0..segments {
            let segment = state
                .get_segment(index)
                .and_then(|segment| segment.to_str().ok().map(|text| text.to_string()))
                .ok_or_else(|| format!("failed to read whisper segment text at index {index}"))?;
            if !transcript.is_empty() {
                transcript.push(' ');
            }
            transcript.push_str(segment.trim());
        }

        Ok(transcript.trim().to_string())
    }

    /// Windows ARM stub: whisper-rs-sys's vendored C++ doesn't build on
    /// `aarch64-pc-windows-msvc`. Mic capture and the F3 state machine
    /// still ship; only transcription is unavailable. The test-bypass
    /// path (`CLUD_VOICE_TEST_TRANSCRIPT`) is preserved so unit tests
    /// for the state machine still run on this target.
    #[cfg(all(target_arch = "aarch64", target_os = "windows"))]
    fn transcribe_audio(
        config: &VoiceConfig,
        _context: &mut Option<WhisperContextHandle>,
        _audio: &[f32],
    ) -> Result<String, String> {
        if let Some(test_transcript) = &config.test_transcript {
            return Ok(test_transcript.clone());
        }
        Err("voice transcription is not supported on this platform \
             (aarch64-pc-windows-msvc): the vendored whisper-rs-sys C++ \
             source does not build on Windows ARM"
            .to_string())
    }

    #[derive(Debug, Clone, Copy)]
    enum CueTone {
        Start,
        Stop,
    }

    #[cfg(not(target_os = "linux"))]
    fn play_cue(tone: CueTone) {
        std::thread::spawn(move || {
            let freq_hz = match tone {
                CueTone::Start => 880.0,
                CueTone::Stop => 660.0,
            };
            let duration = match tone {
                CueTone::Start => Duration::from_millis(90),
                CueTone::Stop => Duration::from_millis(120),
            };

            match OutputStreamBuilder::open_default_stream() {
                Ok(stream) => {
                    let sink = Sink::connect_new(stream.mixer());
                    let source = rodio::source::SineWave::new(freq_hz)
                        .take_duration(duration)
                        .amplify(0.20);
                    sink.append(source);
                    sink.sleep_until_end();
                    drop(stream);
                }
                Err(_) => {
                    print!("\x07");
                    let _ = io::stdout().flush();
                }
            }
        });
    }

    #[cfg(target_os = "linux")]
    fn play_cue(_tone: CueTone) {
        print!("\x07");
        let _ = io::stdout().flush();
    }

    fn missing_model_message() -> String {
        format!(
            "voice mode is enabled but the Whisper model is not yet available. Either:\n  \
             - set CLUD_WHISPER_MODEL to a ggml-small.en.bin path, or\n  \
             - wait for the auto-download to finish (cache: {:?})",
            model::default_cache_path()
        )
    }

    pub struct VoiceMode {
        config: VoiceConfig,
        recording: Option<ActiveRecording>,
        worker: Option<VoiceWorker>,
        transcribing: bool,
        missing_model_reported: bool,
        /// Auto-download completion signal. Set to true by the background
        /// download thread when the model is ready (or has failed and
        /// won't retry). Probed on each F3 press so we re-check the
        /// cache once the download lands without polling on every tick.
        download_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// Cached model path once resolution succeeds. Avoids re-hashing
        /// a 466 MB file every time the user starts a recording.
        resolved_model: Option<PathBuf>,
    }

    impl VoiceMode {
        pub fn from_env() -> Self {
            let config = VoiceConfig::from_env();
            let download_done = model::fresh_completion_flag();

            // Eagerly resolve the model on startup. Three cases:
            //   1. Voice is disabled → skip everything (don't probe network).
            //   2. CLUD_VOICE_TEST_TRANSCRIPT is set → tests don't touch
            //      Whisper at all; skip model setup.
            //   3. A copy is already cached → resolve synchronously, no
            //      thread spawned.
            //   4. Model is missing → spawn the background download
            //      thread so the user can press F3 sooner.
            let mut resolved_model: Option<PathBuf> = None;
            if config.enabled && config.test_transcript.is_none() {
                resolved_model = model::resolve_if_available(config.model_path.as_deref());
                if resolved_model.is_none() {
                    model::ensure_downloaded_in_background(Arc::clone(&download_done));
                } else {
                    // Already on disk — short-circuit the "download done"
                    // flag so the first F3 press doesn't print a "still
                    // downloading" warning.
                    download_done.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }

            Self {
                config,
                recording: None,
                worker: None,
                transcribing: false,
                missing_model_reported: false,
                download_done,
                resolved_model,
            }
        }

        /// Re-attempt to resolve a model path if we don't have one yet
        /// AND the background download has finished. Mutates
        /// `self.resolved_model` and (on success) `self.config.model_path`
        /// so downstream transcription can pick it up without further
        /// plumbing.
        fn refresh_resolved_model(&mut self) {
            if self.resolved_model.is_some() {
                return;
            }
            if !self.download_done.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            if let Some(path) = model::resolve_if_available(self.config.model_path.as_deref()) {
                // Make the path visible to the worker via the existing
                // `model_path` field. The worker takes a `VoiceConfig`
                // snapshot when it spawns (see `VoiceWorker::spawn`), so
                // we also have to nuke and respawn the worker if it
                // captured a stale (None) path. Simpler than threading
                // shared state: drop self.worker so the next
                // `stop_recording` lazily spawns a fresh one.
                if self.config.model_path.is_none() {
                    self.worker = None;
                }
                self.config.model_path = Some(path.clone());
                self.resolved_model = Some(path);
            }
        }

        fn start_recording(&mut self) {
            if !self.config.enabled {
                return;
            }

            // Probe for download completion in case a model just landed.
            self.refresh_resolved_model();

            if self.resolved_model.is_none() && self.config.test_transcript.is_none() {
                if !self.missing_model_reported {
                    eprintln!("[clud] voice: {}", missing_model_message());
                    self.missing_model_reported = true;
                }
                return;
            }

            if self.transcribing || self.recording.is_some() {
                return;
            }

            // `CLUD_VOICE_TEST_TRANSCRIPT` bypasses the mic entirely so the
            // integration test in `tests/integration/test_voice_mode.py`
            // runs on CI hosts with no audio device (Linux runners with no
            // ALSA card, macOS runners where the mic is too tightly
            // sandboxed for the timing window). The release path will
            // still queue a transcription; the worker short-circuits to
            // `config.test_transcript` and writes it back into the PTY.
            if self.config.test_transcript.is_some() {
                play_cue(CueTone::Start);
                self.recording = Some(ActiveRecording::synthetic());
                return;
            }

            match ActiveRecording::start() {
                Ok(recording) => {
                    play_cue(CueTone::Start);
                    self.recording = Some(recording);
                }
                Err(err) => eprintln!("[clud] voice: {err}"),
            }
        }

        fn stop_recording(&mut self) {
            let Some(recording) = self.recording.take() else {
                return;
            };

            play_cue(CueTone::Stop);
            match recording.finish() {
                Ok(audio) if audio.is_empty() => {}
                Ok(audio) => {
                    let worker = self
                        .worker
                        .get_or_insert_with(|| VoiceWorker::spawn(self.config.clone()));
                    if let Err(err) = worker.request_transcription(audio) {
                        eprintln!("[clud] voice: {err}");
                        self.transcribing = false;
                    } else {
                        self.transcribing = true;
                    }
                }
                Err(err) => eprintln!("[clud] voice: {err}"),
            }
        }

        fn drain_worker_events(&mut self, process: &NativePtyProcess) -> io::Result<()> {
            let Some(worker) = self.worker.as_ref() else {
                return Ok(());
            };

            loop {
                match worker.try_recv() {
                    Ok(WorkerEvent::Transcript(text)) => {
                        self.transcribing = false;
                        let trimmed = text.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        process
                            .write_impl(trimmed.as_bytes(), false)
                            .map_err(|err| io::Error::other(err.to_string()))?;
                    }
                    Ok(WorkerEvent::Error(err)) => {
                        self.transcribing = false;
                        eprintln!("[clud] voice: {err}");
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.transcribing = false;
                        break;
                    }
                }
            }
            Ok(())
        }
    }

    impl InteractiveHooks for VoiceMode {
        fn intercept_f3(&self) -> bool {
            self.config.enabled
        }

        /// F3 press starts recording if idle; otherwise it's a no-op.
        /// Toggle semantics (press-to-stop) used to live here but lose
        /// to the hold-to-record contract — a press while already
        /// recording either means autorepeat (terminal sent the SS3-R
        /// sequence twice) or the user re-pressed without an
        /// intervening release, neither of which should kill capture.
        /// Stops come from `on_f3_release` (kitty terminals) or the VAD
        /// auto-stop in `on_tick` (everyone else).
        fn on_f3_press(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
            if self.recording.is_none() {
                self.start_recording();
            }
            Ok(())
        }

        fn on_f3_release(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
            if self.recording.is_some() {
                self.stop_recording();
            }
            Ok(())
        }

        fn on_tick(&mut self, process: &NativePtyProcess) -> io::Result<()> {
            // VAD-silence / hard-cap auto-stop. Fires on terminals that
            // don't emit kitty release events (Windows ConPTY in
            // particular); harmless on those that do, because
            // `on_f3_release` will have already drained `self.recording`.
            let auto_stop = self
                .recording
                .as_mut()
                .map(|rec| rec.should_auto_stop())
                .unwrap_or(false);
            if auto_stop {
                self.stop_recording();
            }
            self.drain_worker_events(process)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn voice_config_can_enable_from_model_path_without_flag() {
            let _guard = EnvGuard::set("CLUD_WHISPER_MODEL", "C:\\models\\ggml-small.en.bin");
            let config = VoiceConfig::from_env();
            assert!(config.enabled);
            assert!(config.model_path.is_some());
        }

        #[test]
        fn downmix_and_resample_handles_stereo() {
            let audio = vec![1.0, -1.0, 0.5, -0.5];
            let output = downmix_and_resample(audio, 2, TARGET_SAMPLE_RATE);
            assert_eq!(output, vec![0.0, 0.0]);
        }

        #[test]
        fn silence_detection_works() {
            assert!(is_effectively_silent(&[0.0, 0.001, -0.002]));
            assert!(!is_effectively_silent(&[0.5]));
        }

        #[test]
        fn has_speech_peak_matches_silence_threshold() {
            // The VAD branch uses `has_speech_peak`; it must use the same
            // threshold as `is_effectively_silent`. If they drift, the
            // recording will either auto-stop while the user is still
            // speaking, or fail to auto-stop on long silences.
            assert!(!has_speech_peak(&[0.0, 0.001, -0.002]));
            assert!(has_speech_peak(&[0.0, 0.5, 0.0]));
            assert!(!has_speech_peak(&[]));
        }

        // ─── Model resolution ─────────────────────────────────────────
        // Issue #13: auto-downloader. Tests cover the no-network code
        // paths — actual downloads are exercised manually since CI has
        // no business pulling 466 MB per platform per job.

        #[test]
        fn model_resolver_uses_env_override_when_present() {
            // Write a tempfile and point CLUD_WHISPER_MODEL at it. The
            // resolver must return that exact path with no hash check.
            let dir = tempfile::tempdir().expect("tempdir");
            let fake_model = dir.path().join("user-model.bin");
            std::fs::write(&fake_model, b"not actually a model").expect("write");
            let resolved = model::resolve_if_available(Some(&fake_model));
            assert_eq!(resolved.as_deref(), Some(fake_model.as_path()));
        }

        #[test]
        fn model_resolver_returns_none_when_nothing_present() {
            // Env override points at a non-existent path AND no
            // (validated) cache copy. The resolver must report "not
            // available" so the caller can kick off the download.
            let dir = tempfile::tempdir().expect("tempdir");
            let bogus = dir.path().join("does-not-exist.bin");
            // `default_cache_path` may legitimately exist on a dev
            // machine; this test only asserts that an unreachable
            // override doesn't get returned.
            let resolved = model::resolve_if_available(Some(&bogus));
            assert_ne!(resolved.as_deref(), Some(bogus.as_path()));
        }

        // ─── Hold-to-record semantics ─────────────────────────────────
        // These exercise the F3 state machine WITHOUT touching the
        // audio device or Whisper. The mic/transcription paths are
        // mocked out via `CLUD_VOICE_TEST_TRANSCRIPT` which short-
        // circuits the worker.

        /// Helper: build a VoiceMode in a known good state for state-
        /// machine tests. The test-transcript bypass keeps Whisper and
        /// the network out of the test.
        fn voice_mode_for_state_test() -> (VoiceMode, EnvGuard, EnvGuard) {
            let model_guard = EnvGuard::set("CLUD_WHISPER_MODEL", "/dev/null");
            let test_guard = EnvGuard::set("CLUD_VOICE_TEST_TRANSCRIPT", "mock-text");
            let mode = VoiceMode::from_env();
            (mode, model_guard, test_guard)
        }

        #[test]
        fn voice_state_press_while_recording_is_ignored() {
            // Hold-to-record contract: a press WHILE already recording
            // must NOT toggle the recording off. This is the load-
            // bearing test against the old toggle behavior.
            //
            // The recording slot is filled via `ActiveRecording::synthetic`
            // — no real cpal stream, no audio device. The older helper
            // built a real WASAPI stream and dropped it without `.play()`,
            // which segfaulted `cargo test` on the Windows server runners
            // whose phantom default input device returned a stream that
            // crashed on drop (STATUS_ACCESS_VIOLATION).
            let (mut mode, _g1, _g2) = voice_mode_for_state_test();
            assert!(mode.recording.is_none(), "starts idle");

            mode.recording = Some(ActiveRecording::synthetic());
            let pty_handle = make_dummy_pty();
            let result = <VoiceMode as crate::session::InteractiveHooks>::on_f3_press(
                &mut mode,
                &pty_handle,
            );
            assert!(result.is_ok());
            assert!(
                mode.recording.is_some(),
                "press while recording must NOT stop the capture (was the old toggle behavior)"
            );
        }

        #[test]
        fn voice_state_release_stops_recording() {
            let (mut mode, _g1, _g2) = voice_mode_for_state_test();
            mode.recording = Some(ActiveRecording::synthetic());
            let pty_handle = make_dummy_pty();
            let _ = <VoiceMode as crate::session::InteractiveHooks>::on_f3_release(
                &mut mode,
                &pty_handle,
            );
            assert!(
                mode.recording.is_none(),
                "release must drain the recording slot so the next press can start fresh"
            );
        }

        #[test]
        fn voice_state_release_when_idle_is_noop() {
            // The pump fires `on_f3_release` whenever the byte stream
            // says so, even if we never saw the matching press. Must
            // tolerate that without panicking.
            let (mut mode, _g1, _g2) = voice_mode_for_state_test();
            assert!(mode.recording.is_none());
            let pty_handle = make_dummy_pty();
            let result = <VoiceMode as crate::session::InteractiveHooks>::on_f3_release(
                &mut mode,
                &pty_handle,
            );
            assert!(result.is_ok());
        }

        fn make_dummy_pty() -> NativePtyProcess {
            // We never call any method on this handle in these tests —
            // the hooks only touch `process.write_impl` from the
            // transcript-injection path, which we deliberately don't
            // exercise here. `NativePtyProcess::new` is non-blocking
            // and accepts any argv; it doesn't spawn until
            // `start_impl` is called.
            let argv = vec!["true".to_string()];
            NativePtyProcess::new(argv, None, None, 24, 80, None).expect("pty handle")
        }

        struct EnvGuard {
            key: &'static str,
            previous: Option<String>,
        }

        impl EnvGuard {
            fn set(key: &'static str, value: &str) -> Self {
                let previous = env::var(key).ok();
                unsafe { env::set_var(key, value) };
                Self { key, previous }
            }
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.previous {
                    Some(value) => unsafe { env::set_var(self.key, value) },
                    None => unsafe { env::remove_var(self.key) },
                }
            }
        }
    }
}

pub use enabled::VoiceMode;
