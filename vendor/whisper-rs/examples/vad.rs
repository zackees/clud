use hound::{SampleFormat, WavSpec, WavWriter};
use std::io::Read;
use std::time::Instant;
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams, WhisperVadSegment};

fn main() {
    let model_path = std::env::args()
        .nth(1)
        .expect("Please specify path to VAD model as argument 1");
    let wav_path = std::env::args()
        .nth(2)
        .expect("Please specify path to WAV file as argument 2");
    let dest_path = std::env::args()
        .nth(3)
        .expect("Please specify output path as argument 3");

    let wav_reader = hound::WavReader::open(wav_path).expect("failed to open wav file");
    let input_sample_rate = wav_reader.spec().sample_rate;
    let input_channels = wav_reader.spec().channels;
    assert_eq!(input_sample_rate, 16000, "expected 16kHz sample rate");
    assert_eq!(input_channels, 1, "expected mono audio");

    let samples = decode_to_float(wav_reader);

    let mut vad_ctx_params = WhisperVadContextParams::default();
    vad_ctx_params.set_n_threads(1);
    vad_ctx_params.set_use_gpu(false);

    // Note this context could be held in a global Mutex or similar
    // There's no restrictions on where the output can be sent after it's used,
    // as it just holds a C-style array internally with no references to the model.
    let mut vad_ctx =
        WhisperVadContext::new(&model_path, vad_ctx_params).expect("failed to load model");

    let vad_params = WhisperVadParams::new();
    let st = Instant::now();
    let result = vad_ctx
        .segments_from_samples(vad_params, &samples)
        .expect("failed to run VAD");
    let et = Instant::now();
    let dt = et.duration_since(st);
    println!("took {:?} to run the VAD model", dt);

    let mut output = WavWriter::create(
        dest_path,
        WavSpec {
            channels: input_channels,
            sample_rate: 16000,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        },
    )
    .expect("failed to open output file");
    for WhisperVadSegment { start, end } in result {
        // convert from centiseconds to seconds
        let start_ts = start / 100.0;
        let end_ts = end / 100.0;
        println!("detected speech between {}s and {}s", start_ts, end_ts);

        let start_sample_idx = (start_ts * input_sample_rate as f32) as usize;
        let end_sample_idx = (end_ts * input_sample_rate as f32) as usize;
        for sample in &samples[start_sample_idx..end_sample_idx] {
            output
                .write_sample(*sample)
                .expect("failed to write sample");
        }
    }
    output.finalize().expect("failed to finalize dest file");
}

fn decode_to_float<T: Read>(rdr: hound::WavReader<T>) -> Vec<f32> {
    match rdr.spec().sample_format {
        SampleFormat::Float => rdr
            .into_samples::<f32>()
            .map(|x| x.expect("expected fp32 WAV file"))
            .collect(),
        SampleFormat::Int => rdr
            .into_samples::<i16>()
            .map(|x| x.expect("expected i16 WAV file") as f32 / 32768.0)
            .collect(),
    }
}
