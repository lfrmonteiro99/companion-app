use anyhow::Result;

use crate::aggregator::ContextEvent;
use crate::api::{FilterResponse, OpenAiClient};
use crate::api_vision::VisionClient;
use crate::config::Config;

/// Analysis backend. Two implementations share the same FilterResponse shape
/// so gate/eval/JSONL layers stay unchanged regardless of choice.
pub enum Backend {
    Text(OpenAiClient),
    Vision(VisionClient),
}

impl Backend {
    pub fn new(kind: BackendKind, cfg: &Config) -> Result<Self> {
        Ok(match kind {
            BackendKind::Text => Backend::Text(OpenAiClient::new(cfg)?),
            BackendKind::Vision => Backend::Vision(VisionClient::new(cfg)?),
        })
    }

    /// True when the backend requires the raw screenshot for analysis. The
    /// OCR loop uses this to decide whether to keep the image cached.
    pub fn needs_image(&self) -> bool {
        matches!(self, Backend::Vision(_))
    }

    /// Analyze the current tick. `image_png` is ignored for Text, required
    /// for Vision (returns an error if missing). `reason` is the gate
    /// reason — the vision backend uses it to pick between cheap and sharp
    /// models.
    pub async fn analyze(
        &self,
        event: &ContextEvent,
        image_png: Option<&[u8]>,
        memory_summary: &str,
        reason: &str,
    ) -> Result<FilterResponse> {
        match self {
            Backend::Text(c)   => c.filter_call(event, memory_summary).await,
            Backend::Vision(c) => match image_png {
                Some(b) => c.analyze_with_image(event, b, memory_summary, reason).await,
                None    => anyhow::bail!("vision backend called without image"),
            },
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Backend::Text(_) => "text",
            Backend::Vision(_) => "vision",
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum BackendKind {
    Text,
    Vision,
}
