use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[cfg(feature = "audio")]
use chrono::{DateTime, Utc};

use crate::config::Config;

pub use awareness_core::types::AudioChunk;

/// Spawns microphone capture loop.
///
/// Logic:
/// 1. Open default cpal input device at 16kHz mono i16.
/// 2. Buffer samples in a ring buffer.
/// 3. Every 30ms, run VAD on the frame.
/// 4. When voice detected: start accumulating.
/// 5. When 1.5s of silence after voice: emit AudioChunk.
/// 6. Hard split at 30s regardless.
///
/// All cpal/webrtc-vad code behind #[cfg(feature = "full")].
/// Without the feature: spawn a task that sleeps forever (no-op).
pub async fn spawn_mic_capture(
    tx: mpsc::Sender<AudioChunk>,
    _cfg: Arc<Config>,
) -> Result<JoinHandle<()>> {
    #[cfg(feature = "full")]
    {
        return spawn_mic_capture_full(tx).await;
    }

    #[cfg(not(feature = "full"))]
    {
        let _ = tx;
        tracing::warn!(
            "Audio capture disabled: built without --features full. \
             Mic transcripts will be empty. \
             Rebuild with 'cargo build --features full' to enable."
        );
        let handle = tokio::spawn(async {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
            }
        });
        Ok(handle)
    }
}

// ---------------------------------------------------------------------------
// Full path (feature = "full")
// ---------------------------------------------------------------------------

#[cfg(feature = "full")]
async fn spawn_mic_capture_full(tx: mpsc::Sender<AudioChunk>) -> Result<JoinHandle<()>> {
    // cpal stream is !Send — build everything inside the thread.
    // Use a oneshot to signal startup errors back to the caller.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

    std::thread::spawn(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use std::sync::{Arc as StdArc, Mutex};

        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(d) => d,
            None => {
                let _ = ready_tx.send(Err(anyhow::anyhow!("No default input device")));
                return;
            }
        };

        tracing::info!("Audio device: {}", device.name().unwrap_or_default());

        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(16_000),
            buffer_size: cpal::BufferSize::Default,
        };

        let (cpal_tx, cpal_rx) = std::sync::mpsc::channel::<Vec<i16>>();
        let error_flag = StdArc::new(Mutex::new(Option::<String>::None));
        let error_flag_cb = StdArc::clone(&error_flag);

        let stream = match device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let _ = cpal_tx.send(data.to_vec());
            },
            move |err| {
                *error_flag_cb.lock().unwrap() = Some(err.to_string());
            },
            None,
        ) {
            Ok(s) => s,
            Err(e) => {
                let _ = ready_tx.send(Err(anyhow::anyhow!("build_input_stream: {e}")));
                return;
            }
        };

        if let Err(e) = stream.play() {
            let _ = ready_tx.send(Err(anyhow::anyhow!("stream.play: {e}")));
            return;
        }

        let _ = ready_tx.send(Ok(()));
        let _stream = stream; // keep alive
        vad_loop(cpal_rx, tx, error_flag);
    });

    // Wait for startup signal
    ready_rx.recv()??;

    let handle = tokio::spawn(async {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
        }
    });
    Ok(handle)
}

// ---------------------------------------------------------------------------
// VAD loop — runs in spawn_blocking thread
// ---------------------------------------------------------------------------

#[cfg(feature = "full")]
fn vad_loop(
    cpal_rx: std::sync::mpsc::Receiver<Vec<i16>>,
    tx: mpsc::Sender<AudioChunk>,
    error_flag: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) {
    use webrtc_vad::{Vad, VadMode};

    const SAMPLE_RATE: u32 = 16_000;
    // 30ms frame at 16kHz = 480 samples.
    const FRAME_SAMPLES: usize = 480;
    // 1.5s of silence = 1500ms / 30ms = 50 frames.
    const SILENCE_FRAMES_THRESHOLD: usize = 50;
    // Hard split at 30s = 30 * 16000 samples.
    const HARD_SPLIT_SAMPLES: usize = 30 * SAMPLE_RATE as usize;

    let mut vad = Vad::new();
    vad.set_mode(VadMode::Quality);

    let mut ring: Vec<i16> = Vec::new();
    let mut accumulator: Vec<i16> = Vec::new();
    let mut in_speech = false;
    let mut silence_frames = 0usize;
    let mut chunk_start: DateTime<Utc> = Utc::now();

    loop {
        // Check for cpal errors.
        if let Ok(guard) = error_flag.lock() {
            if let Some(ref msg) = *guard {
                tracing::warn!("cpal stream error: {}", msg);
                break;
            }
        }

        // Drain incoming samples into ring buffer.
        while let Ok(samples) = cpal_rx.try_recv() {
            ring.extend_from_slice(&samples);
        }

        // Process frames.
        while ring.len() >= FRAME_SAMPLES {
            let frame: Vec<i16> = ring.drain(..FRAME_SAMPLES).collect();

            let is_voice = vad.is_voice_segment(&frame).unwrap_or(false);

            if is_voice {
                if !in_speech {
                    in_speech = true;
                    silence_frames = 0;
                    chunk_start = Utc::now();
                    accumulator.clear();
                    tracing::info!("VAD: speech started");
                }
                silence_frames = 0;
                accumulator.extend_from_slice(&frame);
            } else if in_speech {
                silence_frames += 1;
                accumulator.extend_from_slice(&frame);

                let silence_triggered = silence_frames >= SILENCE_FRAMES_THRESHOLD;
                let hard_split = accumulator.len() >= HARD_SPLIT_SAMPLES;

                if silence_triggered || hard_split {
                    emit_chunk(&tx, chunk_start, &accumulator);
                    in_speech = false;
                    silence_frames = 0;
                    accumulator.clear();
                }
            } else if accumulator.len() >= HARD_SPLIT_SAMPLES {
                // Hard split even without confirmed speech start.
                emit_chunk(&tx, chunk_start, &accumulator);
                accumulator.clear();
                chunk_start = Utc::now();
            }
        }

        // Brief sleep to avoid busy-spinning.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(feature = "full")]
fn emit_chunk(tx: &mpsc::Sender<AudioChunk>, started_at: DateTime<Utc>, samples: &[i16]) {
    const SAMPLE_RATE: f32 = 16_000.0;
    let duration_secs = samples.len() as f32 / SAMPLE_RATE;

    let chunk = AudioChunk {
        started_at,
        samples: samples.to_vec(),
        duration_secs,
    };

    if tx.try_send(chunk).is_err() {
        tracing::warn!("AudioChunk channel full — dropping chunk");
    } else {
        tracing::info!("AudioChunk emitted: {:.2}s", duration_secs);
    }
}
