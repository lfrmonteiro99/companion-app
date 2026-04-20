//! Persistent user profile that feeds the system prompt with context
//! the model can't infer from a single tick.
//!
//! Three sources contribute:
//!  * `bio`: free-text the user types in the Android UI ("I'm a Rust
//!    engineer in Lisbon, job-hunting EU remote").
//!  * Rating feedback: when the user taps "mais disto" / "menos disto"
//!    on a notification, the excerpt is appended to `interests` /
//!    `anti_interests` (deduplicated, short).
//!  * Passive heuristics: every tick the service increments an app
//!    usage counter; top-N apps surface as "most-used apps" hints.
//!
//! Serialised to `filesDir/user_profile.json` on every update.
//! Reads cheap (cached in memory after first load).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAX_INTERESTS: usize = 40;
const MAX_ANTI_INTERESTS: usize = 40;
const MAX_INTEREST_LEN: usize = 160;
const TOP_APPS_IN_PROMPT: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserProfile {
    /// User-written biography. Free-form, injected verbatim.
    #[serde(default)]
    pub bio: String,
    /// Topics the user explicitly marked "more of this".
    #[serde(default)]
    pub interests: Vec<String>,
    /// Topics the user explicitly marked "not interested".
    #[serde(default)]
    pub anti_interests: Vec<String>,
    /// Package name → tick count. Passive, reset on app reinstall.
    #[serde(default)]
    pub app_usage: HashMap<String, u32>,
    /// Unix epoch seconds of last mutation, for prompt freshness.
    #[serde(default)]
    pub updated_at: i64,
}

impl UserProfile {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn set_bio(&mut self, bio: String) {
        self.bio = bio;
        self.touch();
    }

    pub fn record_app_usage(&mut self, app: &str) {
        if app.is_empty() {
            return;
        }
        *self.app_usage.entry(app.to_string()).or_insert(0) += 1;
        self.touch();
    }

    pub fn add_interest(&mut self, topic: &str) {
        let trimmed = normalise(topic);
        if trimmed.is_empty() {
            return;
        }
        // Remove from anti if present — a later "mais disto" overrides
        // an earlier "menos disto" on the same topic.
        self.anti_interests.retain(|s| s != &trimmed);
        if !self.interests.iter().any(|s| s == &trimmed) {
            self.interests.push(trimmed);
            if self.interests.len() > MAX_INTERESTS {
                self.interests.drain(0..self.interests.len() - MAX_INTERESTS);
            }
        }
        self.touch();
    }

    pub fn add_anti_interest(&mut self, topic: &str) {
        let trimmed = normalise(topic);
        if trimmed.is_empty() {
            return;
        }
        self.interests.retain(|s| s != &trimmed);
        if !self.anti_interests.iter().any(|s| s == &trimmed) {
            self.anti_interests.push(trimmed);
            if self.anti_interests.len() > MAX_ANTI_INTERESTS {
                self.anti_interests
                    .drain(0..self.anti_interests.len() - MAX_ANTI_INTERESTS);
            }
        }
        self.touch();
    }

    fn touch(&mut self) {
        self.updated_at = chrono::Utc::now().timestamp();
    }

    /// Formatted block to prepend to the model's system content.
    /// Empty when nothing meaningful exists yet, so early-run prompts
    /// aren't polluted with a bare "Apps mais usadas: " trailing colon.
    pub fn to_prompt_context(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let bio = self.bio.trim();
        if !bio.is_empty() {
            parts.push(format!("Sobre o utilizador: {bio}"));
        }
        if !self.interests.is_empty() {
            parts.push(format!(
                "Interesses confirmados pelo utilizador: {}",
                self.interests.join(", ")
            ));
        }
        if !self.anti_interests.is_empty() {
            parts.push(format!(
                "Tópicos que o utilizador NÃO quer ver em alertas: {}",
                self.anti_interests.join(", ")
            ));
        }
        let top = self.top_apps(TOP_APPS_IN_PROMPT);
        if !top.is_empty() {
            parts.push(format!("Apps mais usadas recentemente: {}", top.join(", ")));
        }
        if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n")
        }
    }

    fn top_apps(&self, n: usize) -> Vec<String> {
        let mut v: Vec<(&String, &u32)> = self.app_usage.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.into_iter().take(n).map(|(k, _)| k.clone()).collect()
    }
}

fn normalise(s: &str) -> String {
    let t = s.trim().trim_matches('"');
    if t.chars().count() > MAX_INTEREST_LEN {
        t.chars().take(MAX_INTEREST_LEN).collect()
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_profile_produces_empty_context() {
        assert_eq!(UserProfile::default().to_prompt_context(), "");
    }

    #[test]
    fn bio_alone_shows_in_context() {
        let mut p = UserProfile::default();
        p.set_bio("engineer".into());
        assert!(p.to_prompt_context().contains("Sobre o utilizador: engineer"));
    }

    #[test]
    fn add_interest_moves_from_anti() {
        let mut p = UserProfile::default();
        p.add_anti_interest("memes");
        p.add_interest("memes");
        assert!(p.interests.contains(&"memes".to_string()));
        assert!(!p.anti_interests.contains(&"memes".to_string()));
    }

    #[test]
    fn app_usage_counts_and_sorts() {
        let mut p = UserProfile::default();
        for _ in 0..3 {
            p.record_app_usage("chrome");
        }
        p.record_app_usage("teams");
        let top = p.top_apps(2);
        assert_eq!(top[0], "chrome");
        assert_eq!(top[1], "teams");
    }
}
