use std::io::Cursor;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio_util::sync::CancellationToken;
use zdx_core::config::Config;

use crate::events::{RecordedAudio, UiEvent};

const CAPTURE_FILENAME: &str = "voice_input.wav";
const CAPTURE_MIME: &str = "audio/wav";

pub async fn voice_record(cancel: Option<CancellationToken>) -> UiEvent {
    let result = match cancel {
        Some(token) => tokio::task::spawn_blocking(move || record_audio_blocking(token))
            .await
            .map_err(|e| e.to_string())
            .and_then(|result| result.map_err(|e| e.to_string())),
        None => Err("voice recording requires a cancellation token".to_string()),
    };
    UiEvent::VoiceRecorded { result }
}

pub async fn voice_transcribe(
    config: Config,
    audio: RecordedAudio,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    let transcription = config.telegram.transcription.clone();
    let result = zdx_core::audio::transcribe::transcribe_audio_if_configured(
        &config,
        &transcription,
        audio.bytes,
        &audio.filename,
        Some(&audio.mime_type),
        cancel.as_ref(),
    )
    .await
    .map_err(|e| e.to_string());
    UiEvent::VoiceTranscribed { result }
}

fn record_audio_blocking(token: CancellationToken) -> Result<RecordedAudio> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("No microphone input device available")?;
    let supported = device
        .default_input_config()
        .context("No supported microphone input format available")?;
    let sample_rate = supported.sample_rate().0;
    let channels = usize::from(supported.channels());

    let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
    let stream_errors = Arc::new(Mutex::new(None::<String>));
    let config = supported.config();

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build_input_stream_f32(
            &device,
            &config,
            channels,
            Arc::clone(&samples),
            Arc::clone(&stream_errors),
        )?,
        cpal::SampleFormat::I16 => build_input_stream_i16(
            &device,
            &config,
            channels,
            Arc::clone(&samples),
            Arc::clone(&stream_errors),
        )?,
        cpal::SampleFormat::U16 => build_input_stream_u16(
            &device,
            &config,
            channels,
            Arc::clone(&samples),
            Arc::clone(&stream_errors),
        )?,
        other => return Err(anyhow!("Unsupported microphone sample format: {other:?}")),
    };

    stream
        .play()
        .context("Failed to start microphone capture")?;
    tokio::runtime::Handle::current().block_on(token.cancelled_owned());
    drop(stream);

    if let Some(err) = stream_errors
        .lock()
        .expect("stream errors lock poisoned")
        .take()
    {
        return Err(anyhow!(err));
    }

    let samples = samples.lock().expect("samples lock poisoned").clone();
    if samples.is_empty() {
        return Err(anyhow!("No audio captured. Try speaking a bit longer."));
    }

    let bytes = encode_wav(&samples, sample_rate)?;
    Ok(RecordedAudio {
        bytes,
        filename: CAPTURE_FILENAME.to_string(),
        mime_type: CAPTURE_MIME.to_string(),
    })
}

fn build_input_stream_f32(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    samples: Arc<Mutex<Vec<i16>>>,
    stream_errors: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream> {
    device
        .build_input_stream(
            config,
            move |data: &[f32], _| push_frames_f32(&samples, data, channels),
            move |err| {
                *stream_errors.lock().expect("stream errors lock poisoned") = Some(err.to_string());
            },
            None,
        )
        .context("build f32 microphone stream")
}

fn build_input_stream_i16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    samples: Arc<Mutex<Vec<i16>>>,
    stream_errors: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream> {
    device
        .build_input_stream(
            config,
            move |data: &[i16], _| push_frames_i16(&samples, data, channels),
            move |err| {
                *stream_errors.lock().expect("stream errors lock poisoned") = Some(err.to_string());
            },
            None,
        )
        .context("build i16 microphone stream")
}

fn build_input_stream_u16(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    samples: Arc<Mutex<Vec<i16>>>,
    stream_errors: Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream> {
    device
        .build_input_stream(
            config,
            move |data: &[u16], _| push_frames_u16(&samples, data, channels),
            move |err| {
                *stream_errors.lock().expect("stream errors lock poisoned") = Some(err.to_string());
            },
            None,
        )
        .context("build u16 microphone stream")
}

fn push_frames_f32(samples: &Arc<Mutex<Vec<i16>>>, data: &[f32], channels: usize) {
    let mut output = samples.lock().expect("samples lock poisoned");
    for frame in data.chunks(channels) {
        let sample = frame.first().copied().unwrap_or(0.0).clamp(-1.0, 1.0);
        output.push((sample * f32::from(i16::MAX)) as i16);
    }
}

fn push_frames_i16(samples: &Arc<Mutex<Vec<i16>>>, data: &[i16], channels: usize) {
    let mut output = samples.lock().expect("samples lock poisoned");
    for frame in data.chunks(channels) {
        output.push(*frame.first().unwrap_or(&0));
    }
}

fn push_frames_u16(samples: &Arc<Mutex<Vec<i16>>>, data: &[u16], channels: usize) {
    let mut output = samples.lock().expect("samples lock poisoned");
    for frame in data.chunks(channels) {
        let sample = i32::from(*frame.first().unwrap_or(&0)) - 32_768;
        output.push(sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
    }
}

fn encode_wav(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::new(&mut cursor, spec).context("create WAV writer")?;
    for sample in samples {
        writer.write_sample(*sample).context("write WAV sample")?;
    }
    writer.finalize().context("finalize WAV file")?;
    Ok(cursor.into_inner())
}
