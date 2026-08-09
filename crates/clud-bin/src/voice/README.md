# voice/

F3 push-to-talk voice mode (issue #13). Captures microphone audio while F3 is held, plays start/stop cues, and writes the transcript back into the backend PTY at the user's cursor. Microphone capture uses `cpal` on Windows and macOS and `arecord` (alsa-utils) on Linux. Transcription is stubbed on every platform: `whisper-rs` was removed after its vendored C++ CMake build (`whisper-rs-sys`) repeatedly broke Windows host builds (stale CMake generator cache colliding with plain `cargo build`); `worker.rs::transcribe_audio` always returns an "unavailable" error outside the `CLUD_VOICE_TEST_TRANSCRIPT` bypass.

## Files

- `mod.rs` — module root; documents the F3 flow and re-exports `VoiceMode`.
- `mode.rs` — `VoiceMode` state machine and `InteractiveHooks` impl (press/release/tick).
- `config.rs` — `VoiceConfig::from_env` reads `CLUD_VOICE`, `CLUD_WHISPER_MODEL`, `CLUD_VOICE_LANGUAGE`, `CLUD_VOICE_TEST_TRANSCRIPT`.
- `recording.rs` — `ActiveRecording`: starts/stops the capture backend, runs VAD auto-stop, returns resampled samples.
- `audio.rs` — sample-rate constants, VAD thresholds, mono downmix + linear resample to 16 kHz, silence/speech detection.
- `cues.rs` — `play_cue(CueTone::Start|Stop)`: short sine-wave beep via `rodio` (BEL fallback on Linux).
- `linux_capture.rs` — Linux-only `arecord` subprocess wrapper that streams S16_LE PCM into a shared sample buffer.
- `model.rs` — model resolution, SHA-256-pinned auto-download to per-OS cache dir, background download thread. Kept for a future transcription backend; unrelated to the whisper-rs removal (no C++/CMake dependency).
- `worker.rs` — `VoiceWorker` thread: hands audio to `transcribe_audio`, which is a stub (see module header), sends `WorkerEvent::Transcript|Error`.

## Key items

- `VoiceMode` struct: `mode.rs:16`
- `VoiceMode::from_env` (eager model resolve + background download): `mode.rs:33`
- `InteractiveHooks for VoiceMode` (`on_f3_press`/`on_f3_release`/`on_tick`): `mode.rs:194`
- `VoiceConfig::from_env`: `config.rs:13`
- `ActiveRecording::start` (cpal): `recording.rs:39`
- `ActiveRecording::start` (Linux/arecord): `recording.rs:86`
- `ActiveRecording::synthetic` (test/no-mic bypass): `recording.rs:116`
- `ActiveRecording::should_auto_stop` (VAD + hard cap): `recording.rs:150`
- `ActiveRecording::finish` (drain + resample): `recording.rs:191`
- `downmix_and_resample`: `audio.rs:21`
- `is_effectively_silent` / `has_speech_peak`: `audio.rs:59`, `audio.rs:66`
- VAD constants (`MIN_CAPTURE_MS`, `MAX_SILENCE_PEAK`, `MIN_VAD_AFTER_SPEECH_MS`, `VAD_SILENCE_TAIL_MS`, `MAX_CAPTURE_MS`): `audio.rs:5`-`audio.rs:19`
- `play_cue` + `CueTone`: `cues.rs:9`, `cues.rs:15`
- `LinuxCapture::start` / `LinuxCapture::stop`: `linux_capture.rs:23`, `linux_capture.rs:74`
- `resolve_if_available`: `model.rs:53`
- `ensure_downloaded_in_background`: `model.rs:72`
- `default_cache_path`, `MODEL_FILENAME`, `MODEL_SHA256`: `model.rs:42`, `model.rs:26`, `model.rs:34`
- `VoiceWorker::spawn` / `request_transcription` / `try_recv`: `worker.rs:23`, `worker.rs:48`, `worker.rs:54`
- `WorkerEvent` enum: `worker.rs:11`
- `transcribe_audio` (stub) and `missing_model_message`: `worker.rs:65`, `worker.rs:72`

## Used by

- `crate::runner` (`runner.rs:22`, `runner.rs:525`) constructs `voice::VoiceMode::from_env()` and passes it as the `InteractiveHooks` for the interactive PTY session.
- `crate::session::InteractiveHooks` is the trait `VoiceMode` implements; the session pump in `session/` calls `on_f3_press`, `on_f3_release`, and `on_tick`, and the `F3Observer` in `session/` produces the press/release events that drive this module.
