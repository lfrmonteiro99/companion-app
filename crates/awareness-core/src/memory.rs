use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub timestamp: DateTime<Utc>,
    pub app: Option<String>,
    pub alert_type: String,
    pub should_alert: bool,
    pub quick_message: String,
}

pub struct MemoryRing {
    buf: VecDeque<MemoryEntry>,
    capacity: usize,
}

impl MemoryRing {
    pub fn new(capacity: usize) -> Self {
        Self { buf: VecDeque::with_capacity(capacity), capacity }
    }
    pub fn push(&mut self, e: MemoryEntry) {
        if self.buf.len() == self.capacity { self.buf.pop_front(); }
        self.buf.push_back(e);
    }
    /// Oldest first, newest last. Empty string when ring is empty.
    pub fn to_prompt_lines(&self) -> String {
        self.buf.iter().map(|e| {
            let when = e.timestamp.format("%H:%M");
            let app  = e.app.as_deref().unwrap_or("?");
            format!(
                "[{when}] {app} alert={} ({}): {:?}",
                e.should_alert, e.alert_type, e.quick_message
            )
        }).collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mk(msg: &str) -> MemoryEntry {
        MemoryEntry {
            timestamp: Utc::now(),
            app: Some("x".into()),
            alert_type: "none".into(),
            should_alert: false,
            quick_message: msg.into(),
        }
    }
    #[test]
    fn evicts_oldest_when_full() {
        let mut r = MemoryRing::new(3);
        for i in 0..5 { r.push(mk(&i.to_string())); }
        let s = r.to_prompt_lines();
        assert!(s.contains("\"2\"") && s.contains("\"3\"") && s.contains("\"4\""));
        assert!(!s.contains("\"0\"") && !s.contains("\"1\""));
    }
    #[test]
    fn empty_ring_produces_empty_string() {
        assert_eq!(MemoryRing::new(5).to_prompt_lines(), "");
    }
}
