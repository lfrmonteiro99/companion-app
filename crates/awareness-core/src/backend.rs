use anyhow::Result;

use crate::api::LlmClient;
use crate::config::Config;
use crate::types::{ContextEvent, FilterResponse};

pub enum Backend {
    Text {
        client: LlmClient,
        wants_image: bool,
    },
}

impl Backend {
    pub fn new(kind: BackendKind, cfg: &Config) -> Result<Self> {
        Ok(match kind {
            BackendKind::Text => Backend::Text {
                client: LlmClient::new(cfg)?,
                wants_image: cfg.vision_enabled,
            },
        })
    }

    pub fn needs_image(&self) -> bool {
        match self {
            Backend::Text { wants_image, .. } => *wants_image,
        }
    }

    pub async fn analyze(
        &self,
        event: &ContextEvent,
        image_png: Option<&[u8]>,
        memory_summary: &str,
        _reason: &str,
        user_profile: &str,
        matched_interests: &[String],
    ) -> Result<FilterResponse> {
        match self {
            Backend::Text {
                client,
                wants_image,
            } => {
                let img = if *wants_image { image_png } else { None };
                client
                    .filter_call(event, memory_summary, user_profile, matched_interests, img)
                    .await
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Backend::Text { .. } => "text",
        }
    }
    pub fn max_cost_estimate_usd(&self) -> f64 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_image_follows_config() {
        let mut cfg = Config::for_android(String::new(), 0.0);
        cfg.vision_enabled = true;
        let b = Backend::new(BackendKind::Text, &cfg).unwrap();
        assert!(b.needs_image());
        cfg.vision_enabled = false;
        let b2 = Backend::new(BackendKind::Text, &cfg).unwrap();
        assert!(!b2.needs_image());
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum, Default)]
pub enum BackendKind {
    #[default]
    Text,
}
