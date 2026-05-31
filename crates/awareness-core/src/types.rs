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
    /// Transcript of currently-playing system audio (e.g. a reel's
    /// soundtrack) from the audio monitor source. `None` when the
    /// media-audio lane is off. Populated in sub-phase 1b.
    #[serde(default)]
    pub media_audio_text: Option<String>,
}

/// Structured response from the filter / gate API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterResponse {
    pub should_alert: bool,
    pub alert_type: String, // "focus"|"time_spent"|"emotional"|"preparation"|"voice_reply"|"none"
    pub urgency: String,    // "low"|"medium"|"high"
    pub needs_deep_analysis: bool,
    pub quick_message: String,
    /// A ready-to-use reply the user can copy/paste/insert (chats, voice_reply).
    /// `None` when no reply is applicable.
    #[serde(default)]
    pub suggested_reply: Option<String>,
    /// A concrete next action the user could take (a CTA). `None` when none.
    #[serde(default)]
    pub suggested_action: Option<String>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub cost_usd: f64,
    /// Set when the model's response could not be parsed as the expected JSON
    /// schema. Tokens were still spent — caller should deduct `cost_usd` but
    /// must NOT treat other fields as meaningful signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    /// Interests from the user's explicit list that matched the screen on
    /// this tick (subset of what was sent in the user turn). Populated by
    /// the core before calling the model so the client can show which of
    /// the curated interests were actually fed to the LLM.
    #[serde(default)]
    pub matched_interests: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_event_legacy_json_deserializes_without_media_audio() {
        let legacy = r#"{"timestamp":"2026-05-29T10:00:00Z","app":"vscode","window_title":null,
            "screen_text_excerpt":"x","mic_text_recent":null,"duration_on_app_seconds":5,
            "history_apps_30min":[]}"#;
        let ev: ContextEvent = serde_json::from_str(legacy).unwrap();
        assert_eq!(ev.media_audio_text, None);
        assert!(!ev.mic_text_new);
    }

    #[test]
    fn filter_response_legacy_json_suggested_fields_default_to_none() {
        // JSON persisted by previous versions has no suggested_reply /
        // suggested_action — #[serde(default)] must keep old JSONL readable.
        let legacy = r#"{
          "should_alert": true,
          "alert_type": "voice_reply",
          "urgency": "medium",
          "needs_deep_analysis": false,
          "quick_message": "João há 9 min: PR #142 pronto?",
          "tokens_in": 10,
          "tokens_out": 20,
          "cost_usd": 0.0
        }"#;
        let r: crate::types::FilterResponse = serde_json::from_str(legacy).unwrap();
        assert_eq!(r.suggested_reply, None);
        assert_eq!(r.suggested_action, None);
    }

    #[test]
    fn filter_response_round_trips_suggested_fields() {
        let r = crate::types::FilterResponse {
            should_alert: true,
            alert_type: "voice_reply".into(),
            urgency: "medium".into(),
            needs_deep_analysis: false,
            quick_message: "João há 9 min: PR pronto?".into(),
            suggested_reply: Some("Ainda na review, fecho antes das 18h.".into()),
            suggested_action: Some("Responde no Teams".into()),
            tokens_in: 5,
            tokens_out: 10,
            cost_usd: 0.0,
            parse_error: None,
            matched_interests: Vec::new(),
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: crate::types::FilterResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(
            back.suggested_reply.as_deref(),
            Some("Ainda na review, fecho antes das 18h.")
        );
        assert_eq!(back.suggested_action.as_deref(), Some("Responde no Teams"));
    }

    #[test]
    fn context_event_round_trips_media_audio() {
        let json = r#"{"timestamp":"2026-05-29T10:00:00Z","app":"Instagram","window_title":null,
            "screen_text_excerpt":"","mic_text_recent":null,"duration_on_app_seconds":1,
            "history_apps_30min":[],"media_audio_text":"upbeat music, voice says hi"}"#;
        let ev: ContextEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            ev.media_audio_text.as_deref(),
            Some("upbeat music, voice says hi")
        );
        assert!(serde_json::to_string(&ev)
            .unwrap()
            .contains("media_audio_text"));
    }
}

impl FilterResponse {
    /// Convenience for constructing a "nothing happened" response (gate
    /// skip, budget exceeded, api error) without spelling out every
    /// field in every call site.
    pub fn short_circuit(alert_type: impl Into<String>, quick_message: impl Into<String>) -> Self {
        Self {
            should_alert: false,
            alert_type: alert_type.into(),
            urgency: "low".into(),
            needs_deep_analysis: false,
            quick_message: quick_message.into(),
            suggested_reply: None,
            suggested_action: None,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
            parse_error: None,
            matched_interests: Vec::new(),
        }
    }
}
