//! In-process audio for [`crate::voice`]: microphone capture through cpal
//! and cue tones through rodio.
//!
//! Windows and macOS share this file: `platform/unix/voice/mod.rs`
//! includes it with `#[path]` on macOS. Linux uses the `arecord` helper in
//! `platform/unix/voice/linux_capture.rs` instead, so libasound is never
//! loaded into the main process there.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{OutputStreamBuilder, Sink, Source};

/// Play a sine cue of `freq_hz` for `duration` on a background thread,
/// falling back to the terminal bell when no output device opens.
pub fn play_cue(freq_hz: f32, duration: Duration) {
    std::thread::spawn(move || match OutputStreamBuilder::open_default_stream() {
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
    });
}

/// A live microphone capture appending `f32` samples to a shared buffer.
pub struct MicCapture {
    stream: cpal::Stream,
}

impl MicCapture {
    /// Start capturing from the default input device into `samples`.
    /// Returns the capture with the device's sample rate and channel
    /// count.
    pub fn start(samples: Arc<Mutex<Vec<f32>>>) -> Result<(Self, u32, u16), String> {
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

        Ok((Self { stream }, sample_rate, channels))
    }

    /// Stop the capture by dropping the stream.
    pub fn stop(self) -> Result<(), String> {
        drop(self.stream);
        Ok(())
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
