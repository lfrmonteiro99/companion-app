//! Text-to-speech delivery for voice-driven alerts.
//!
//! Detects a local TTS binary at config time (user override → spd-say → espeak-ng
//! → espeak → say) and spawns it non-blocking so alert dispatch is never held
//! up by audio playback. When no backend is available, `speak` no-ops — the
//! caller can still rely on notify-send.

use tokio::process::Command;

/// Resolved TTS backend. Construct once at startup via `TtsConfig::resolve`
/// and pass by reference into `speak`.
#[derive(Debug, Clone)]
pub struct TtsConfig {
    pub enabled: bool,
    /// The binary to invoke. `None` when TTS is disabled or no backend
    /// was found on PATH.
    pub command: Option<String>,
}

impl TtsConfig {
    /// Resolve the TTS backend. If `override_cmd` is set and exists on PATH,
    /// use it. Otherwise probe common backends in priority order.
    pub fn resolve(enabled: bool, override_cmd: Option<&str>) -> Self {
        if !enabled {
            return Self {
                enabled: false,
                command: None,
            };
        }

        if let Some(cmd) = override_cmd {
            if binary_exists(cmd) {
                return Self {
                    enabled: true,
                    command: Some(cmd.to_string()),
                };
            }
            tracing::warn!(
                "tts: configured binary {cmd:?} not on PATH; falling back to auto-detect"
            );
        }

        // spd-say is the default on most Linux desktops (speech-dispatcher).
        // espeak-ng/espeak work headless. `say` is macOS built-in.
        for candidate in ["spd-say", "espeak-ng", "espeak", "say"] {
            if binary_exists(candidate) {
                return Self {
                    enabled: true,
                    command: Some(candidate.to_string()),
                };
            }
        }

        tracing::warn!(
            "tts: no TTS backend found (spd-say/espeak-ng/espeak/say); audio alerts disabled"
        );
        Self {
            enabled: true,
            command: None,
        }
    }
}

/// Speak `text` using the resolved backend. Returns immediately — playback
/// runs in the background. Silently no-ops when disabled or unresolved.
pub fn speak(text: &str, cfg: &TtsConfig) {
    if !cfg.enabled {
        return;
    }
    let Some(cmd) = cfg.command.as_deref() else {
        return;
    };
    let trimmed = shorten_for_tts(text);
    if trimmed.is_empty() {
        return;
    }

    // spd-say is async by default (returns immediately after queueing); the
    // others block for the full utterance. Either way we don't await.
    let args = args_for(cmd, &trimmed);
    let result = Command::new(cmd)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn();
    if let Err(e) = result {
        tracing::warn!("tts: failed to spawn {cmd}: {e}");
    }
}

/// Keep TTS utterances short: first sentence, capped at 220 chars. Strips
/// code fences and backticks that read terribly aloud.
fn shorten_for_tts(text: &str) -> String {
    let mut cleaned = text.replace('`', "");
    cleaned = cleaned.replace("```", "");

    // First sentence — split on the first terminator.
    let end = cleaned
        .find(['.', '!', '?'])
        .map(|i| i + 1)
        .unwrap_or(cleaned.len());
    let first = cleaned[..end].trim().to_string();

    if first.chars().count() > 220 {
        first.chars().take(220).collect::<String>()
    } else {
        first
    }
}

fn args_for(cmd: &str, text: &str) -> Vec<String> {
    // spd-say's last positional is the text. The others take the text as the
    // final argument too. macOS `say` is the same. Keep it simple.
    let _ = cmd; // kept for future per-backend flag tuning
    vec![text.to_string()]
}

fn binary_exists(cmd: &str) -> bool {
    // `which` returns 0 when found. We do this synchronously at startup only
    // (TtsConfig::resolve is called once), so std::process is fine.
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_picks_first_sentence() {
        let s = shorten_for_tts("Hello world. Second sentence ignored. Third.");
        assert_eq!(s, "Hello world.");
    }

    #[test]
    fn shorten_strips_backticks() {
        let s = shorten_for_tts("Use `cargo build` now.");
        assert_eq!(s, "Use cargo build now.");
    }

    #[test]
    fn shorten_caps_length() {
        let long = "a".repeat(500);
        let s = shorten_for_tts(&long);
        assert_eq!(s.chars().count(), 220);
    }

    #[test]
    fn shorten_handles_no_terminator() {
        let s = shorten_for_tts("no punctuation here");
        assert_eq!(s, "no punctuation here");
    }

    #[test]
    fn shorten_empty_stays_empty() {
        assert_eq!(shorten_for_tts(""), "");
        assert_eq!(shorten_for_tts("   "), "");
    }

    #[test]
    fn speak_disabled_noops() {
        let cfg = TtsConfig {
            enabled: false,
            command: Some("echo".into()),
        };
        speak("hello", &cfg); // must not panic, must not spawn
    }

    #[test]
    fn speak_no_command_noops() {
        let cfg = TtsConfig {
            enabled: true,
            command: None,
        };
        speak("hello", &cfg);
    }

    #[test]
    fn resolve_disabled_returns_no_command() {
        let c = TtsConfig::resolve(false, None);
        assert!(!c.enabled);
        assert!(c.command.is_none());
    }

    #[test]
    fn resolve_missing_override_falls_back_to_autodetect() {
        // "definitely-not-on-path-xyz" doesn't exist; resolve must not panic
        // and must either find a real backend or return command=None.
        let c = TtsConfig::resolve(true, Some("definitely-not-on-path-xyz-123"));
        assert!(c.enabled);
        // command is whatever auto-detect finds on this box, which may be
        // nothing in CI — both outcomes are fine.
        let _ = c.command;
    }
}
