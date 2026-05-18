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

mod audio;
mod config;
mod cues;
#[cfg(target_os = "linux")]
mod linux_capture;
mod mode;
mod model;
mod recording;
mod worker;

pub use mode::VoiceMode;
