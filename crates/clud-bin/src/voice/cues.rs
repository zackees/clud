use std::io::{self, Write};
#[cfg(not(target_os = "linux"))]
use std::time::Duration;

#[cfg(not(target_os = "linux"))]
use rodio::{OutputStreamBuilder, Sink, Source};

#[derive(Debug, Clone, Copy)]
pub(super) enum CueTone {
    Start,
    Stop,
}

#[cfg(not(target_os = "linux"))]
pub(super) fn play_cue(tone: CueTone) {
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
pub(super) fn play_cue(_tone: CueTone) {
    print!("\x07");
    let _ = io::stdout().flush();
}
