use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use crate::types::ContextEvent;
use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateDecision {
    pub action: GateAction,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GateAction {
    Send,
    Skip,
}

#[derive(Debug, Default)]
pub struct GateState {
    pub last_app: Option<String>,
    pub last_sent_at: Option<DateTime<Utc>>,
    /// Screen text excerpt from the most recent Send. Used by the
    /// "text_changed" rule to detect new words the user has typed/seen since.
    pub last_sent_text: Option<String>,
    /// Timestamp of the most recent voice_activity send. Tracked separately
    /// from last_sent_at so the short voice cooldown doesn't block other rules.
    pub last_voice_send: Option<DateTime<Utc>>,
}

/// Count distinct whitespace-split tokens in `current` that are not present in
/// `previous`. Cheap proxy for "how much new content appeared" — works well
/// for typing into a compose box where the surrounding UI text stays constant.
fn new_words_count(current: &str, previous: &str) -> usize {
    use std::collections::HashSet;
    let prev: HashSet<&str> = previous.split_whitespace().collect();
    current.split_whitespace().filter(|w| !prev.contains(w)).count()
}

/// Pure function: given event + state + config → decision + updated state.
pub fn evaluate(
    event: &ContextEvent,
    state: &mut GateState,
    cfg: &Config,
) -> GateDecision {
    // Rule 1: App changed.
    if event.app != state.last_app {
        state.last_app = event.app.clone();
        state.last_sent_at = Some(Utc::now());
        state.last_sent_text = Some(event.screen_text_excerpt.clone());
        return GateDecision {
            action: GateAction::Send,
            reason: "app_change".to_string(),
        };
    }

    // Rule 2: Time on app exceeds threshold.
    let threshold_seconds = cfg.gate_app_time_threshold_minutes * 60;
    if event.duration_on_app_seconds >= threshold_seconds {
        state.last_sent_at = Some(Utc::now());
        state.last_sent_text = Some(event.screen_text_excerpt.clone());
        return GateDecision {
            action: GateAction::Send,
            reason: "time_on_app".to_string(),
        };
    }

    // Rule 3: Frustration keyword detected.
    let mic_lower = event
        .mic_text_recent
        .as_deref()
        .unwrap_or("")
        .to_lowercase();
    let screen_lower = event.screen_text_excerpt.to_lowercase();
    let has_frustration = cfg
        .gate_frustration_keywords
        .iter()
        .any(|kw| {
            let kw_lower = kw.to_lowercase();
            mic_lower.contains(&kw_lower) || screen_lower.contains(&kw_lower)
        });

    if has_frustration {
        state.last_sent_at = Some(Utc::now());
        state.last_sent_text = Some(event.screen_text_excerpt.clone());
        return GateDecision {
            action: GateAction::Send,
            reason: "emotional".to_string(),
        };
    }

    // Rule 4: Screen text changed significantly — user typed new content into
    // the currently focused app. Covers the "I wrote something and paused"
    // case that doesn't trigger app_change/emotional/periodic soon enough.
    if cfg.gate_text_new_words_threshold > 0 {
        let cooldown = Duration::seconds(cfg.gate_text_change_cooldown_seconds as i64);
        let cooldown_elapsed = match state.last_sent_at {
            None => false, // don't fire before anything has been sent at all
            Some(last) => Utc::now() - last >= cooldown,
        };
        if cooldown_elapsed && !event.screen_text_excerpt.is_empty() {
            let prev = state.last_sent_text.as_deref().unwrap_or("");
            let new_words = new_words_count(&event.screen_text_excerpt, prev);
            if new_words >= cfg.gate_text_new_words_threshold {
                state.last_sent_at = Some(Utc::now());
                state.last_sent_text = Some(event.screen_text_excerpt.clone());
                return GateDecision {
                    action: GateAction::Send,
                    reason: "text_changed".to_string(),
                };
            }
        }
    }

    // Rule 5: No alert in last N minutes AND non-empty screen text.
    let periodic_window = Duration::minutes(cfg.gate_periodic_check_minutes as i64);
    let no_recent_alert = match state.last_sent_at {
        None => true,
        Some(last) => Utc::now() - last > periodic_window,
    };

    if no_recent_alert && !event.screen_text_excerpt.is_empty() {
        state.last_sent_at = Some(Utc::now());
        state.last_sent_text = Some(event.screen_text_excerpt.clone());
        return GateDecision {
            action: GateAction::Send,
            reason: "periodic_check".to_string(),
        };
    }

    // Rule 6: Fresh voice activity. Fires when a transcript just arrived and
    // the voice-specific cooldown has elapsed — keeps conversational alerts
    // responsive without spamming the API on every whisper chunk.
    if event.mic_text_new
        && !event.mic_text_recent.as_deref().unwrap_or("").trim().is_empty()
    {
        let voice_cooldown = Duration::seconds(cfg.gate_voice_cooldown_seconds as i64);
        let voice_cooldown_elapsed = match state.last_voice_send {
            None => true,
            Some(last) => Utc::now() - last >= voice_cooldown,
        };
        if voice_cooldown_elapsed {
            let now = Utc::now();
            state.last_sent_at = Some(now);
            state.last_voice_send = Some(now);
            state.last_sent_text = Some(event.screen_text_excerpt.clone());
            return GateDecision {
                action: GateAction::Send,
                reason: "voice_activity".to_string(),
            };
        }
    }

    // Rule 7: No trigger.
    GateDecision {
        action: GateAction::Skip,
        reason: "no_trigger".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_event() -> ContextEvent {
        ContextEvent {
            timestamp: Utc::now(),
            app: Some("vscode".to_string()),
            window_title: None,
            screen_text_excerpt: "some code here".to_string(),
            mic_text_recent: None,
            duration_on_app_seconds: 0,
            history_apps_30min: vec![],
            mic_text_new: false,
        }
    }

    fn make_config() -> Config {
        Config {
            openai_api_key: "test".to_string(),
            budget_usd_daily: 1.0,
            tick_screen_seconds: 2,
            tick_analysis_seconds: 10,
            whisper_model_path: std::path::PathBuf::from("model.bin"),
            perceptual_hash_threshold: 8,
            text_dedup_similarity: 0.85,
            gate_app_time_threshold_minutes: 25,
            gate_periodic_check_minutes: 15,
            gate_text_new_words_threshold: 5,
            gate_text_change_cooldown_seconds: 6,
            gate_voice_cooldown_seconds: 5,
            gate_frustration_keywords: crate::config_file::default_frustration_keywords(),
            min_send_interval_seconds: 15,
            transcript_window_size: 5,
            tts_enabled: false,
            tts_command: None,
            output_dir: std::path::PathBuf::from("data"),
            log_level: "info".to_string(),
            a11y_script: std::path::PathBuf::from("scripts/a11y_dump.py"),
            backend: crate::backend::BackendKind::Text,
        }
    }

    #[test]
    fn test_app_change_triggers_send() {
        let cfg = make_config();
        let mut state = GateState {
            last_app: Some("firefox".to_string()),
            last_sent_at: Some(Utc::now()),
            last_sent_text: None,
            last_voice_send: None,
        };
        let event = make_event(); // app = "vscode", differs from "firefox"
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Send);
        assert_eq!(decision.reason, "app_change");
        assert_eq!(state.last_app, Some("vscode".to_string()));
    }

    #[test]
    fn test_time_threshold_triggers_send() {
        let cfg = make_config();
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(Utc::now()),
            last_sent_text: None,
            last_voice_send: None,
        };
        let mut event = make_event();
        event.duration_on_app_seconds = 25 * 60; // exactly at threshold
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Send);
        assert_eq!(decision.reason, "time_on_app");
    }

    #[test]
    fn test_frustration_keyword_triggers_send() {
        let cfg = make_config();
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(Utc::now()),
            last_sent_text: None,
            last_voice_send: None,
        };
        let mut event = make_event();
        event.mic_text_recent = Some("wtf is this".to_string());
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Send);
        assert_eq!(decision.reason, "emotional");
    }

    #[test]
    fn test_no_trigger_returns_skip() {
        let cfg = make_config();
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(Utc::now()), // sent just now → no periodic_check
            // Same text as the event → no new words → no text_changed fire
            last_sent_text: Some("some code here".to_string()),
            last_voice_send: None,
        };
        let mut event = make_event();
        event.duration_on_app_seconds = 0; // below threshold
        // No frustration keywords
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Skip);
        assert_eq!(decision.reason, "no_trigger");
    }

    #[test]
    fn test_text_changed_fires_after_cooldown() {
        let cfg = make_config();
        let seven_sec_ago = Utc::now() - Duration::seconds(7);
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(seven_sec_ago),
            last_sent_text: Some("fixed prefix text here".to_string()),
            last_voice_send: None,
        };
        let mut event = make_event();
        event.screen_text_excerpt =
            "fixed prefix text here acrescentei palavras novas diferentes agora mesmo".to_string();
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Send);
        assert_eq!(decision.reason, "text_changed");
    }

    #[test]
    fn test_text_changed_respects_cooldown() {
        let cfg = make_config();
        let two_sec_ago = Utc::now() - Duration::seconds(2);
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(two_sec_ago),
            last_sent_text: Some("fixed prefix text here".to_string()),
            last_voice_send: None,
        };
        let mut event = make_event();
        event.screen_text_excerpt =
            "fixed prefix text here muitas palavras novas completamente diferentes".to_string();
        // Even with 7 new words, cooldown (6s) hasn't elapsed → skip.
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Skip);
    }

    #[test]
    fn test_voice_activity_fires_on_fresh_transcript() {
        let cfg = make_config();
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(Utc::now()), // blocks periodic_check
            last_sent_text: Some("some code here".to_string()),
            last_voice_send: None,
        };
        let mut event = make_event();
        event.mic_text_recent = Some("what's the time complexity of this?".to_string());
        event.mic_text_new = true;
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Send);
        assert_eq!(decision.reason, "voice_activity");
        assert!(state.last_voice_send.is_some());
    }

    #[test]
    fn test_voice_activity_respects_cooldown() {
        let cfg = make_config();
        let two_sec_ago = Utc::now() - Duration::seconds(2);
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(Utc::now()),
            last_sent_text: Some("some code here".to_string()),
            last_voice_send: Some(two_sec_ago),
        };
        let mut event = make_event();
        event.mic_text_recent = Some("another thought".to_string());
        event.mic_text_new = true;
        // voice cooldown is 5s → 2s is not enough → skip.
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Skip);
    }

    #[test]
    fn test_voice_activity_skipped_when_mic_text_new_false() {
        let cfg = make_config();
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(Utc::now()),
            last_sent_text: Some("some code here".to_string()),
            last_voice_send: None,
        };
        let mut event = make_event();
        event.mic_text_recent = Some("stale transcript".to_string());
        event.mic_text_new = false; // periodic tick, not a new transcript
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Skip);
    }

    #[test]
    fn test_periodic_check_after_16_minutes() {
        let cfg = make_config();
        let sixteen_min_ago = Utc::now() - Duration::minutes(16);
        let mut state = GateState {
            last_app: Some("vscode".to_string()),
            last_sent_at: Some(sixteen_min_ago),
            last_sent_text: Some("some code here".to_string()),
            last_voice_send: None,
        };
        let event = make_event(); // screen_text_excerpt is non-empty
        let decision = evaluate(&event, &mut state, &cfg);
        assert_eq!(decision.action, GateAction::Send);
        assert_eq!(decision.reason, "periodic_check");
    }
}
