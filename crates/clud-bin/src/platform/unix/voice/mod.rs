//! Audio for [`crate::voice`] on Unix.
//!
//! Linux records through an `arecord` helper process (`linux_capture`) so
//! libasound is loaded only on demand, and cues are the terminal bell.
//! macOS shares the in-process cpal/rodio implementation with Windows.

#[cfg(target_os = "linux")]
mod linux_capture;

#[cfg(target_os = "macos")]
#[path = "../../windows/voice.rs"]
mod cpal_audio;
#[cfg(target_os = "macos")]
pub use cpal_audio::{play_cue, MicCapture};

#[cfg(target_os = "linux")]
pub use linux::{play_cue, MicCapture};

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::linux_capture::LinuxCapture;
    use crate::voice::audio::TARGET_SAMPLE_RATE;

    /// The terminal bell; Linux plays no tone in-process.
    pub fn play_cue(_freq_hz: f32, _duration: Duration) {
        print!("\x07");
        let _ = io::stdout().flush();
    }

    /// A live `arecord` capture appending 16 kHz mono samples to a shared
    /// buffer.
    pub struct MicCapture {
        capture: LinuxCapture,
    }

    impl MicCapture {
        /// Start capturing into `samples`. Returns the capture with its
        /// sample rate and channel count.
        pub fn start(samples: Arc<Mutex<Vec<f32>>>) -> Result<(Self, u32, u16), String> {
            let capture = LinuxCapture::start(samples)?;
            Ok((Self { capture }, TARGET_SAMPLE_RATE, 1))
        }

        /// Stop the helper process and drain its output.
        pub fn stop(self) -> Result<(), String> {
            self.capture.stop()
        }
    }
}
