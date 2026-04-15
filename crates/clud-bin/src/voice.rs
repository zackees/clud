use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{OutputStreamBuilder, Sink, Source};
use running_process_core::pty::NativePtyProcess;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::session::InteractiveHooks;

const TARGET_SAMPLE_RATE: u32 = 16_000;
const MIN_CAPTURE_MS: u128 = 150;
const MAX_SILENCE_PEAK: f32 = 0.01;

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
    stream: cpal::Stream,
}

impl ActiveRecording {
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
            stream,
        })
    }

    fn finish(self) -> Result<Vec<f32>, String> {
        let elapsed_ms = self.started_at.elapsed().as_millis();
        drop(self.stream);

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
}

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
    samples
        .iter()
        .fold(0.0f32, |max, sample| max.max(sample.abs()))
        < MAX_SILENCE_PEAK
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
            let mut context: Option<WhisperContext> = None;

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

fn transcribe_audio(
    config: &VoiceConfig,
    context: &mut Option<WhisperContext>,
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

#[derive(Debug, Clone, Copy)]
enum CueTone {
    Start,
    Stop,
}

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

fn missing_model_message() -> &'static str {
    "voice mode is enabled but CLUD_WHISPER_MODEL is not set; point it at a ggml-small.en.bin Whisper model"
}

pub struct VoiceMode {
    config: VoiceConfig,
    recording: Option<ActiveRecording>,
    worker: Option<VoiceWorker>,
    transcribing: bool,
    missing_model_reported: bool,
}

impl VoiceMode {
    pub fn from_env() -> Self {
        Self {
            config: VoiceConfig::from_env(),
            recording: None,
            worker: None,
            transcribing: false,
            missing_model_reported: false,
        }
    }

    fn start_recording(&mut self) {
        if !self.config.enabled {
            return;
        }

        if self.config.model_path.is_none() && self.config.test_transcript.is_none() {
            if !self.missing_model_reported {
                eprintln!("[clud] voice: {}", missing_model_message());
                self.missing_model_reported = true;
            }
            return;
        }

        if self.transcribing || self.recording.is_some() {
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

    fn on_f3_press(&mut self, _process: &NativePtyProcess) -> io::Result<()> {
        if self.recording.is_some() {
            // Fallback for terminals that do not emit release events.
            self.stop_recording();
        } else {
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
