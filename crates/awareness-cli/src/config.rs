use std::path::PathBuf;
use anyhow::{Context, Result};
use clap::Args;

use crate::backend::BackendKind;

#[derive(Debug, Clone)]
pub struct Config {
    pub openai_api_key: String,
    pub budget_usd_daily: f64,
    pub tick_screen_seconds: u64,
    pub tick_analysis_seconds: u64,
    pub whisper_model_path: PathBuf,
    pub perceptual_hash_threshold: u32,
    pub text_dedup_similarity: f32,
    pub gate_app_time_threshold_minutes: u64,
    pub gate_periodic_check_minutes: u64,
    pub gate_text_new_words_threshold: usize,
    pub gate_text_change_cooldown_seconds: u64,
    pub output_dir: PathBuf,
    pub log_level: String,
    pub a11y_script: PathBuf,
    pub backend: BackendKind,
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    #[arg(long, default_value = "data/phase_poc")]
    pub output_dir: PathBuf,
    #[arg(long, default_value = "models/ggml-base.bin")]
    pub whisper_model: PathBuf,
    #[arg(long, default_value = "0.5")]
    pub budget: f64,
    #[arg(long, default_value = "2")]
    pub tick_screen_seconds: u64,
    #[arg(long, default_value = "10")]
    pub tick_analysis_seconds: u64,
    #[arg(long, default_value = "info")]
    pub log_level: String,
    #[arg(long, default_value = "2")]
    pub gate_periodic_check_minutes: u64,
    /// Min. new-word count (vs last sent text) to trigger a "text_changed"
    /// send. Lower = more sensitive to typing. 0 disables the rule.
    #[arg(long, default_value = "5")]
    pub gate_text_new_words_threshold: usize,
    /// Min. seconds between text_changed sends.
    #[arg(long, default_value = "6")]
    pub gate_text_change_cooldown_seconds: u64,
    /// Path to the AT-SPI sidecar. Script tries first; OCR is the fallback.
    #[arg(long, default_value = "../../scripts/a11y_dump.py")]
    pub a11y_script: PathBuf,
    /// Analysis backend: `vision` sends the screenshot to gpt-4o-mini vision,
    /// `text` sends the extracted OCR/a11y text only (cheaper, lower quality).
    #[arg(long, value_enum, default_value_t = BackendKind::Vision)]
    pub backend: BackendKind,
}

impl Config {
    pub fn from_env_and_args(args: RunArgs) -> Result<Self> {
        // Load .env if present (don't fail if missing)
        let _ = dotenvy::dotenv();

        let openai_api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY not set. Add to .env or environment.")?;

        if openai_api_key.is_empty() {
            anyhow::bail!("OPENAI_API_KEY is empty");
        }

        Ok(Self {
            openai_api_key,
            budget_usd_daily: args.budget,
            tick_screen_seconds: args.tick_screen_seconds,
            tick_analysis_seconds: args.tick_analysis_seconds,
            whisper_model_path: args.whisper_model,
            perceptual_hash_threshold: 3,
            text_dedup_similarity: 0.99,
            gate_app_time_threshold_minutes: 25,
            gate_periodic_check_minutes: args.gate_periodic_check_minutes,
            gate_text_new_words_threshold: args.gate_text_new_words_threshold,
            gate_text_change_cooldown_seconds: args.gate_text_change_cooldown_seconds,
            output_dir: args.output_dir,
            log_level: args.log_level,
            a11y_script: args.a11y_script,
            backend: args.backend,
        })
    }
}
