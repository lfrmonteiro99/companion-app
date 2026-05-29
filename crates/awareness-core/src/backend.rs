use anyhow::Result;

use crate::api::OpenAiClient;
use crate::config::Config;
use crate::types::{ContextEvent, FilterResponse};

/// Analysis backend. Text-only since the vision path was removed when
/// the project moved to a fully local LLM (no OpenAI quota, no Ollama
/// vision model good enough to replace `gpt-4o`-high on dense UI).
pub enum Backend {
    Text(OpenAiClient),
}

impl Backend {
    pub fn new(kind: BackendKind, cfg: &Config) -> Result<Self> {
        Ok(match kind {
            BackendKind::Text => Backend::Text(OpenAiClient::new(cfg)?),
        })
    }

    /// Vision path is gone — image bytes are never required. Kept for
    /// call-site symmetry with the screen-capture loop, which still
    /// asks whether to cache the latest PNG.
    pub fn needs_image(&self) -> bool {
        false
    }

    /// Analyze the current tick. `_image_png` is ignored.
    pub async fn analyze(
        &self,
        event: &ContextEvent,
        _image_png: Option<&[u8]>,
        memory_summary: &str,
        _reason: &str,
        user_profile: &str,
        matched_interests: &[String],
    ) -> Result<FilterResponse> {
        match self {
            Backend::Text(c) => {
                c.filter_call(event, memory_summary, user_profile, matched_interests)
                    .await
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Backend::Text(_) => "text",
        }
    }

    /// Local LLM is free at runtime, so the budget controller's
    /// reservation is always zero. Kept as a method so callers keep
    /// compiling without ripping out the budget plumbing.
    pub fn max_cost_estimate_usd(&self) -> f64 {
        0.0
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum, Default)]
pub enum BackendKind {
    #[default]
    Text,
}
