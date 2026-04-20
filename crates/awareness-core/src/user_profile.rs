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

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

const MAX_INTERESTS: usize = 40;
const MAX_ANTI_INTERESTS: usize = 40;
const MAX_INTEREST_LEN: usize = 160;
const TOP_APPS_IN_PROMPT: usize = 6;
const MAX_EXPLICIT_INTERESTS: usize = 80;
const MIN_PATTERN_LEN: usize = 3;
const TOP_K_FILTERED: usize = 12;

#[derive(Debug, Serialize, Deserialize, Default)]
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
    /// User-curated topic tags entered through the Android settings
    /// UI. Orthogonal to `interests` (which is rating-learned) and to
    /// `anti_interests`. Used to produce a per-tick filtered list
    /// that the prompt injects only when the screen actually mentions
    /// something that matches.
    #[serde(default)]
    pub explicit_interests: Vec<String>,
    /// Aho-Corasick matcher over `explicit_interests`. Rebuilt on
    /// mutation; never persisted.
    #[serde(skip)]
    matcher: OnceLock<Option<AhoCorasick>>,
}

impl Clone for UserProfile {
    // OnceLock doesn't derive Clone; rebuild lazily on the clone.
    fn clone(&self) -> Self {
        Self {
            bio: self.bio.clone(),
            interests: self.interests.clone(),
            anti_interests: self.anti_interests.clone(),
            app_usage: self.app_usage.clone(),
            updated_at: self.updated_at,
            explicit_interests: self.explicit_interests.clone(),
            matcher: OnceLock::new(),
        }
    }
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

    // ── Explicit (curated) interests ─────────────────────────────

    pub fn list_explicit_interests(&self) -> &[String] {
        &self.explicit_interests
    }

    /// Replace the whole list at once — matches the Kotlin UI model
    /// where the user sees all pills and taps Save. Dedupes + trims.
    pub fn set_explicit_interests(&mut self, items: Vec<String>) {
        let mut out: Vec<String> = Vec::new();
        for raw in items {
            let t = normalise(&raw);
            if t.chars().count() < MIN_PATTERN_LEN {
                continue;
            }
            if out.iter().any(|s| s.eq_ignore_ascii_case(&t)) {
                continue;
            }
            out.push(t);
            if out.len() >= MAX_EXPLICIT_INTERESTS {
                break;
            }
        }
        self.explicit_interests = out;
        self.matcher = OnceLock::new();
        self.touch();
    }

    pub fn add_explicit_interest(&mut self, topic: &str) -> bool {
        let t = normalise(topic);
        if t.chars().count() < MIN_PATTERN_LEN
            || self.explicit_interests.len() >= MAX_EXPLICIT_INTERESTS
            || self
                .explicit_interests
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&t))
        {
            return false;
        }
        self.explicit_interests.push(t);
        self.matcher = OnceLock::new();
        self.touch();
        true
    }

    pub fn remove_explicit_interest(&mut self, topic: &str) -> bool {
        let before = self.explicit_interests.len();
        self.explicit_interests
            .retain(|s| !s.eq_ignore_ascii_case(topic));
        if self.explicit_interests.len() == before {
            return false;
        }
        self.matcher = OnceLock::new();
        self.touch();
        true
    }

    fn matcher(&self) -> &Option<AhoCorasick> {
        self.matcher.get_or_init(|| {
            if self.explicit_interests.is_empty() {
                return None;
            }
            AhoCorasickBuilder::new()
                .ascii_case_insensitive(true)
                .match_kind(MatchKind::LeftmostLongest)
                .build(self.explicit_interests.iter().map(|s| s.as_str()))
                .ok()
        })
    }

    /// Per-tick interest filter. Weights title + app 3× against screen.
    /// Returns top-K explicit interests whose score exceeds a minimal
    /// threshold; empty when nothing matches so the prompt stays clean.
    pub fn filter_interests_for_screen(
        &self,
        screen: &str,
        window_title: Option<&str>,
        app: Option<&str>,
    ) -> Vec<String> {
        let Some(ac) = self.matcher() else {
            return Vec::new();
        };
        // Haystack: app + title repeated 3× so specific signals beat
        // generic body text. Screen trimmed to 4000 chars (first+last
        // halves keep chrome + fresh content).
        let haystack = build_haystack(screen, window_title, app);
        let mut scores: HashMap<usize, (u32, f32)> = HashMap::new(); // pattern_id → (hits, score)
        for m in ac.find_iter(&haystack) {
            let entry = scores.entry(m.pattern().as_usize()).or_insert((0, 0.0));
            entry.0 += 1;
            // log-damp repetitions so a log file with 500× "error"
            // doesn't flood the ranking.
            entry.1 = 1.0 + (entry.0 as f32).ln_1p();
        }
        if scores.is_empty() {
            return Vec::new();
        }
        let mut ranked: Vec<(usize, u32, f32)> = scores
            .into_iter()
            .map(|(id, (hits, score))| (id, hits, score))
            .collect();
        // Higher score first, then shorter label first (more specific).
        ranked.sort_by(|a, b| {
            b.2.partial_cmp(&a.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| self.explicit_interests[a.0].len().cmp(&self.explicit_interests[b.0].len()))
        });
        ranked
            .into_iter()
            .take(TOP_K_FILTERED)
            .map(|(id, _, _)| self.explicit_interests[id].clone())
            .collect()
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

/// Assemble the text the AhoCorasick matcher scans. Window title and
/// app package name get repeated three times at the front so that a
/// tokio match in the title outscores a tokio mention in a random
/// line of log. Screen text is capped to first 2k + last 2k chars to
/// keep allocation bounded on huge a11y trees.
fn build_haystack(screen: &str, window_title: Option<&str>, app: Option<&str>) -> String {
    let mut out = String::with_capacity(512 + screen.len().min(4096));
    for _ in 0..3 {
        if let Some(t) = window_title {
            out.push_str(t);
            out.push('\n');
        }
        if let Some(a) = app {
            out.push_str(a);
            out.push('\n');
        }
    }
    if screen.chars().count() <= 4000 {
        out.push_str(screen);
    } else {
        let head: String = screen.chars().take(2000).collect();
        let tail: String = screen
            .chars()
            .skip(screen.chars().count() - 2000)
            .collect();
        out.push_str(&head);
        out.push_str("\n…\n");
        out.push_str(&tail);
    }
    out
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
    fn filter_interests_matches_tokens_in_screen() {
        let mut p = UserProfile::default();
        p.set_explicit_interests(vec![
            "Rust programming".into(),
            "tokio".into(),
            "kubernetes".into(),
            "EU remote jobs".into(),
        ]);
        let out = p.filter_interests_for_screen(
            "use tokio::sync::Mutex; fn main() {}",
            Some("main.rs - VS Code"),
            Some("com.visualstudio.code"),
        );
        assert!(out.contains(&"tokio".to_string()));
        assert!(!out.contains(&"kubernetes".to_string()));
    }

    #[test]
    fn filter_returns_empty_when_no_match() {
        let mut p = UserProfile::default();
        p.set_explicit_interests(vec!["rust".into(), "tokio".into()]);
        let out = p.filter_interests_for_screen("weather is sunny today", None, None);
        assert!(out.is_empty());
    }

    #[test]
    fn set_explicit_interests_dedupes_and_rejects_short() {
        let mut p = UserProfile::default();
        p.set_explicit_interests(vec![
            "rust".into(),
            "RUST".into(),
            "ai".into(), // < MIN_PATTERN_LEN
            "tokio".into(),
        ]);
        assert_eq!(p.explicit_interests.len(), 2);
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
