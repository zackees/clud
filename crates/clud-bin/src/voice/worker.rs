use std::sync::mpsc;

// Issue #13 follow-up: whisper-rs-sys does not build on aarch64-pc-windows-msvc.
// The dep is target-gated in Cargo.toml; voice transcription is stubbed
// on that one platform via `WhisperContextHandle = ()` plus an error
// return from `transcribe_audio` (model loading + Whisper calls are
// bypassed there). All other surfaces (mic capture, cue playback,
// F3 state machine, downsampling) ship unchanged.
#[cfg(not(all(target_arch = "aarch64", target_os = "windows")))]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::config::VoiceConfig;
use super::model;

/// On supported targets this aliases the real Whisper context;
/// on Windows ARM (where the dep is omitted) it collapses to `()`
/// so the worker thread compiles without pulling whisper-rs in.
#[cfg(not(all(target_arch = "aarch64", target_os = "windows")))]
type WhisperContextHandle = WhisperContext;
#[cfg(all(target_arch = "aarch64", target_os = "windows"))]
type WhisperContextHandle = ();

#[derive(Debug)]
pub(super) enum WorkerCommand {
    Transcribe { audio: Vec<f32> },
}

#[derive(Debug)]
pub(super) enum WorkerEvent {
    Transcript(String),
    Error(String),
}

#[derive(Debug)]
pub(super) struct VoiceWorker {
    commands: mpsc::Sender<WorkerCommand>,
    events: mpsc::Receiver<WorkerEvent>,
}

impl VoiceWorker {
    pub(super) fn spawn(config: VoiceConfig) -> Self {
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

    pub(super) fn request_transcription(&self, audio: Vec<f32>) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::Transcribe { audio })
            .map_err(|_| "voice worker is unavailable".to_string())
    }

    pub(super) fn try_recv(&self) -> Result<WorkerEvent, mpsc::TryRecvError> {
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

pub(super) fn missing_model_message() -> String {
    format!(
        "voice mode is enabled but the Whisper model is not yet available. Either:\n  \
         - set CLUD_WHISPER_MODEL to a ggml-small.en.bin path, or\n  \
         - wait for the auto-download to finish (cache: {:?})",
        model::default_cache_path()
    )
}
