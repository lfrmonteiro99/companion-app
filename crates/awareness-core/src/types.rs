//! Pure data types shared across platforms.
//!
//! These used to live inside the Linux-specific modules (`ocr`, `audio`,
//! `whisper`, `aggregator`, `api`) in `awareness-cli`. They were moved
//! here so the Android frontend can construct the same events without
//! depending on tesseract / cpal / whisper-rs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Output of an OCR pass over a screen frame. Populated by Tesseract on
/// desktop, by ML Kit Text Recognition on Android, or by an accessibility
/// service when available.
#[derive(Debug, Clone)]
pub struct OcrOutput {
    pub captured_at: DateTime<Utc>,
    pub full_text: String,
    /// OCR of top 60px strip — used to infer app name.
    pub title_bar_text: String,
    pub inferred_app_name: Option<String>,
    /// Active window bounding box in screen pixels (x, y, width, height),
    /// when exposed by the accessibility layer. Callers use it to crop the
    /// full-screen frame to the focused window before OCR/vision, so the
    /// model stops seeing the whole desktop confetti.
    pub active_bbox: Option<(i32, i32, u32, u32)>,
}

/// Raw audio buffer ready for an STT engine.
/// Samples are PCM i16, 16 kHz, mono.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub started_at: DateTime<Utc>,
    pub samples: Vec<i16>,
    pub duration_secs: f32,
}

/// Output of one STT transcription call.
#[derive(Debug, Clone)]
pub struct TranscriptChunk {
    pub started_at: DateTime<Utc>,
    pub text: String,
    /// Detected language code, e.g. "pt" or "en". "unknown" when not available.
    pub language: String,
    /// Approximated confidence in [0.0, 1.0].
    pub confidence: f32,
}

/// The central event type. Passed to the gate, then API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEvent {
    pub timestamp: DateTime<Utc>,
    pub app: Option<String>,
    pub window_title: Option<String>,
    pub screen_text_excerpt: String,
    pub mic_text_recent: Option<String>,
    pub duration_on_app_seconds: u64,
    pub history_apps_30min: Vec<(String, u64)>,
    /// True only when the event was emitted because a fresh transcript just
    /// arrived. Used by the gate's voice_activity rule to distinguish new
    /// speech from stale buffer contents on periodic ticks.
    #[serde(default)]
    pub mic_text_new: bool,
}

/// Structured response from the filter / gate API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterResponse {
    pub should_alert: bool,
    pub alert_type: String, // "focus"|"time_spent"|"emotional"|"preparation"|"voice_reply"|"none"
    pub urgency: String,    // "low"|"medium"|"high"
    pub needs_deep_analysis: bool,
    pub quick_message: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub cost_usd: f64,
    /// Set when the model's response could not be parsed as the expected JSON
    /// schema. Tokens were still spent — caller should deduct `cost_usd` but
    /// must NOT treat other fields as meaningful signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}
