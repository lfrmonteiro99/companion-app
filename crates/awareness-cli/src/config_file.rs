use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub gate: GateSection,
    pub runtime: RuntimeSection,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct GateSection {
    pub frustration_keywords: Option<Vec<String>>,
    pub tuning: GateTuning,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct GateTuning {
    pub app_time_threshold_minutes: Option<u64>,
    pub periodic_check_minutes: Option<u64>,
    pub text_new_words_threshold: Option<usize>,
    pub text_change_cooldown_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeSection {
    pub min_send_interval_seconds: Option<u64>,
}

impl ConfigFile {
    pub fn load_if_present(candidates: &[&Path]) -> Result<Option<Self>> {
        for path in candidates {
            if path.exists() {
                let raw = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {:?}", path))?;
                let parsed: ConfigFile = toml::from_str(&raw)
                    .with_context(|| format!("parsing {:?}", path))?;
                tracing::info!("Loaded config from {:?}", path);
                return Ok(Some(parsed));
            }
        }
        Ok(None)
    }
}

pub fn default_frustration_keywords() -> Vec<String> {
    [
        "não percebo", "não funciona", "por amor", "merda", "caralho",
        "wtf", "why the hell", "this is broken", "not working", "foda-se",
        "impossível", "broken", "crash", "error", "failed",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
