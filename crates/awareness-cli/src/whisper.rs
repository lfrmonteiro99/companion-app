use anyhow::Result;
use std::path::Path;

use crate::audio::AudioChunk;

pub use awareness_core::types::TranscriptChunk;

/// Whisper transcription engine.
#[allow(dead_code)]
pub struct WhisperEngine {
    #[cfg(feature = "full")]
    ctx: whisper_rs::WhisperContext,
    #[cfg(not(feature = "full"))]
    _phantom: (),
}

impl WhisperEngine {
    /// Load a whisper.cpp model from disk.
    ///
    /// With `full` feature: loads the binary model file.
    /// Without `full` feature: returns a no-op stub immediately.
    #[allow(dead_code)]
    pub fn load(model_path: &Path) -> Result<Self> {
        #[cfg(feature = "full")]
        {
            use whisper_rs::WhisperContextParameters;
            let path_str = model_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Model path is not valid UTF-8"))?;
            let ctx =
                whisper_rs::WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
                    .map_err(|e| anyhow::anyhow!("Failed to load whisper model: {e}"))?;
            return Ok(WhisperEngine { ctx });
        }

        #[cfg(not(feature = "full"))]
        {
            let _ = model_path;
            Ok(WhisperEngine { _phantom: () })
        }
    }

    /// Transcribe an audio chunk.
    ///
    /// With `full` feature: runs full whisper inference.
    /// Without `full` feature: returns an empty transcript immediately.
    #[allow(dead_code)]
    pub fn transcribe(&self, chunk: &AudioChunk) -> Result<TranscriptChunk> {
        #[cfg(feature = "full")]
        {
            use whisper_rs::{FullParams, SamplingStrategy};

            let n_threads = (num_cpus::get() / 2).max(1) as i32;

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(None); // auto-detect
            params.set_n_threads(n_threads);
            params.set_print_realtime(false);
            params.set_print_progress(false);
            params.set_print_special(false);
            params.set_print_timestamps(false);

            // Convert i16 PCM → f32 normalised to [-1.0, 1.0].
            let samples_f32: Vec<f32> = chunk
                .samples
                .iter()
                .map(|&s| s as f32 / 32768.0)
                .collect();

            let mut state = self
                .ctx
                .create_state()
                .map_err(|e| anyhow::anyhow!("Failed to create whisper state: {e}"))?;

            state
                .full(params, &samples_f32)
                .map_err(|e| anyhow::anyhow!("Whisper inference failed: {e}"))?;

            let n_segments = state.full_n_segments()
                .map_err(|e| anyhow::anyhow!("full_n_segments failed: {e}"))?;

            let mut text_parts: Vec<String> = Vec::with_capacity(n_segments as usize);
            for i in 0..n_segments {
                if let Ok(seg) = state.full_get_segment_text(i) {
                    text_parts.push(seg.trim().to_string());
                }
            }
            let text = text_parts.join(" ");

            let language = "auto".to_string();

            return Ok(TranscriptChunk {
                started_at: chunk.started_at,
                text,
                language,
                confidence: 0.8,
            });
        }

        #[cfg(not(feature = "full"))]
        {
            Ok(TranscriptChunk {
                started_at: chunk.started_at,
                text: String::new(),
                language: "unknown".into(),
                confidence: 0.0,
            })
        }
    }
}
