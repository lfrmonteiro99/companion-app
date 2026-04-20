use std::time::{Duration, Instant};

/// Tracks how long the user has been focused on the same app. Used by the
/// alert dispatcher to suppress low/medium urgency notifications during deep
/// work — the alert still gets logged to run.log and runs.jsonl, it just
/// doesn't fire a desktop pop-up.
///
/// Rules (simple by design):
///   - Flow is "active" when app has been stable for ≥ STABLE_THRESHOLD and
///     the app is one of the recognised focus apps (editors / terminal).
///   - Any app switch resets the stability timer.
///   - A long gap between updates (user went AFK) also resets.
#[derive(Debug)]
pub struct FlowState {
    last_app: Option<String>,
    last_update_at: Instant,
    app_stable_since: Option<Instant>,
}

const STABLE_THRESHOLD: Duration = Duration::from_secs(5 * 60);
const AFK_GAP: Duration = Duration::from_secs(2 * 60);

const FOCUS_APPS: &[&str] = &[
    "vscode",
    "cursor",
    "code",
    "intellij",
    "pycharm",
    "webstorm",
    "sublime",
    "nvim",
    "neovim",
    "text_editor",
    "terminal",
];

impl FlowState {
    pub fn new() -> Self {
        Self {
            last_app: None,
            last_update_at: Instant::now(),
            app_stable_since: None,
        }
    }

    pub fn update(&mut self, current_app: &Option<String>) {
        let now = Instant::now();
        // User went AFK → reset stability.
        if now.duration_since(self.last_update_at) > AFK_GAP {
            self.app_stable_since = None;
        }
        self.last_update_at = now;

        if current_app != &self.last_app {
            self.last_app = current_app.clone();
            self.app_stable_since = Some(now);
            return;
        }
        if self.app_stable_since.is_none() {
            self.app_stable_since = Some(now);
        }
    }

    pub fn in_flow(&self) -> bool {
        let stable_long_enough = matches!(
            self.app_stable_since,
            Some(t) if t.elapsed() >= STABLE_THRESHOLD
        );
        let focus_app = self.last_app.as_deref().map(is_focus_app).unwrap_or(false);
        stable_long_enough && focus_app
    }
}

fn is_focus_app(app: &str) -> bool {
    let lower = app.to_lowercase();
    FOCUS_APPS.iter().any(|a| lower.contains(a))
}

impl Default for FlowState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_in_flow_initially() {
        let s = FlowState::new();
        assert!(!s.in_flow());
    }

    #[test]
    fn not_in_flow_for_non_focus_app() {
        let mut s = FlowState::new();
        s.update(&Some("teams".to_string()));
        // No way to fast-forward elapsed in unit test without time-travel
        // helpers; still, in_flow must be false because threshold not met.
        assert!(!s.in_flow());
    }

    #[test]
    fn focus_app_recognition() {
        assert!(is_focus_app("vscode"));
        assert!(is_focus_app("VSCODE"));
        assert!(is_focus_app("terminal"));
        assert!(is_focus_app("code"));
        assert!(!is_focus_app("teams"));
        assert!(!is_focus_app("chrome"));
    }
}
