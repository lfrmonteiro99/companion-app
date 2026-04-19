use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use clap::Args;

use crate::backend::BackendKind;
use crate::config_file::{default_frustration_keywords, ConfigFile};

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
    pub gate_frustration_keywords: Vec<String>,
    pub min_send_interval_seconds: u64,
    pub output_dir: PathBuf,
    pub log_level: String,
    pub a11y_script: PathBuf,
    pub backend: BackendKind,
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    #[arg(long)]
    pub output_dir: Option<PathBuf>,
    #[arg(long)]
    pub whisper_model: Option<PathBuf>,
    #[arg(long)]
    pub budget: Option<f64>,
    #[arg(long)]
    pub tick_screen_seconds: Option<u64>,
    #[arg(long)]
    pub tick_analysis_seconds: Option<u64>,
    #[arg(long)]
    pub log_level: Option<String>,
    #[arg(long)]
    pub gate_periodic_check_minutes: Option<u64>,
    /// Min. new-word count (vs last sent text) to trigger a "text_changed"
    /// send. Lower = more sensitive to typing. 0 disables the rule.
    #[arg(long)]
    pub gate_text_new_words_threshold: Option<usize>,
    /// Min. seconds between text_changed sends.
    #[arg(long)]
    pub gate_text_change_cooldown_seconds: Option<u64>,
    /// Path to the AT-SPI sidecar. Script tries first; OCR is the fallback.
    #[arg(long)]
    pub a11y_script: Option<PathBuf>,
    /// Analysis backend: `vision` sends the screenshot to gpt-4o-mini vision,
    /// `text` sends the extracted OCR/a11y text only (cheaper, lower quality).
    #[arg(long, value_enum)]
    pub backend: Option<BackendKind>,
    /// Path to a TOML config file (overrides default search locations).
    #[arg(long)]
    pub config: Option<PathBuf>,
}

fn env_parsed<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
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

        // Load TOML config file (optional). Precedence lowest among non-defaults.
        let toml_cfg = {
            let default_paths: Vec<PathBuf> = match args.config.clone() {
                Some(p) => vec![p],
                None => vec![
                    PathBuf::from("config.toml"),
                    PathBuf::from("./config.toml"),
                ],
            };
            let refs: Vec<&Path> = default_paths.iter().map(|p| p.as_path()).collect();
            ConfigFile::load_if_present(&refs)?.unwrap_or_default()
        };

        // Resolve each value: CLI arg > env var > TOML > hardcoded default.
        let output_dir = args
            .output_dir
            .or_else(|| env_parsed::<PathBuf>("AWARENESS_OUTPUT_DIR"))
            .unwrap_or_else(|| PathBuf::from("data/phase_poc"));

        let whisper_model_path = args
            .whisper_model
            .or_else(|| env_parsed::<PathBuf>("AWARENESS_WHISPER_MODEL"))
            .unwrap_or_else(|| PathBuf::from("models/ggml-base.bin"));

        let budget_usd_daily = args
            .budget
            .or_else(|| env_parsed::<f64>("AWARENESS_BUDGET_USD"))
            .unwrap_or(0.5);

        let tick_screen_seconds = args
            .tick_screen_seconds
            .or_else(|| env_parsed::<u64>("AWARENESS_TICK_SCREEN_SECONDS"))
            .unwrap_or(2);

        let tick_analysis_seconds = args
            .tick_analysis_seconds
            .or_else(|| env_parsed::<u64>("AWARENESS_TICK_ANALYSIS_SECONDS"))
            .unwrap_or(10);

        let log_level = args
            .log_level
            .or_else(|| std::env::var("AWARENESS_LOG_LEVEL").ok())
            .unwrap_or_else(|| "info".to_string());

        let gate_tuning = &toml_cfg.gate.tuning;

        let gate_periodic_check_minutes = args
            .gate_periodic_check_minutes
            .or(gate_tuning.periodic_check_minutes)
            .unwrap_or(2);

        let gate_text_new_words_threshold = args
            .gate_text_new_words_threshold
            .or(gate_tuning.text_new_words_threshold)
            .unwrap_or(5);

        let gate_text_change_cooldown_seconds = args
            .gate_text_change_cooldown_seconds
            .or(gate_tuning.text_change_cooldown_seconds)
            .unwrap_or(6);

        let gate_app_time_threshold_minutes = gate_tuning
            .app_time_threshold_minutes
            .unwrap_or(25);

        let gate_frustration_keywords = toml_cfg
            .gate
            .frustration_keywords
            .clone()
            .unwrap_or_else(default_frustration_keywords);

        let min_send_interval_seconds = toml_cfg
            .runtime
            .min_send_interval_seconds
            .or_else(|| env_parsed::<u64>("AWARENESS_MIN_SEND_INTERVAL_SECONDS"))
            .unwrap_or(15);

        let a11y_script = args
            .a11y_script
            .or_else(|| env_parsed::<PathBuf>("AWARENESS_A11Y_SCRIPT"))
            .unwrap_or_else(|| PathBuf::from("../../scripts/a11y_dump.py"));

        let backend = args
            .backend
            .or_else(|| match std::env::var("AWARENESS_BACKEND").ok().as_deref() {
                Some("text") | Some("Text") => Some(BackendKind::Text),
                Some("vision") | Some("Vision") => Some(BackendKind::Vision),
                _ => None,
            })
            .unwrap_or(BackendKind::Vision);

        let cfg = Self {
            openai_api_key,
            budget_usd_daily,
            tick_screen_seconds,
            tick_analysis_seconds,
            whisper_model_path,
            perceptual_hash_threshold: 3,
            text_dedup_similarity: 0.99,
            gate_app_time_threshold_minutes,
            gate_periodic_check_minutes,
            gate_text_new_words_threshold,
            gate_text_change_cooldown_seconds,
            gate_frustration_keywords,
            min_send_interval_seconds,
            output_dir,
            log_level,
            a11y_script,
            backend,
        };

        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        // Output dir — create if missing (also exercises write permissions).
        std::fs::create_dir_all(&self.output_dir)
            .with_context(|| format!("creating output_dir {:?}", self.output_dir))?;

        // Whisper model is only required when the audio/STT pipeline is compiled.
        #[cfg(feature = "full")]
        if !self.whisper_model_path.exists() {
            anyhow::bail!(
                "Whisper model not found at {:?}. Download with ./scripts/fetch_whisper_model.sh",
                self.whisper_model_path
            );
        }

        // a11y script is optional (OCR is the fallback) but warn if missing.
        if !self.a11y_script.exists() {
            tracing::warn!(
                "a11y script not found at {:?}; OCR will be the only text source",
                self.a11y_script
            );
        }

        if self.budget_usd_daily <= 0.0 {
            anyhow::bail!("--budget must be > 0 (got {})", self.budget_usd_daily);
        }
        if self.tick_screen_seconds == 0 || self.tick_analysis_seconds == 0 {
            anyhow::bail!("tick intervals must be >= 1s");
        }
        if self.tick_analysis_seconds < self.tick_screen_seconds {
            tracing::warn!(
                "tick_analysis_seconds ({}) < tick_screen_seconds ({}); analysis may miss new frames",
                self.tick_analysis_seconds,
                self.tick_screen_seconds
            );
        }
        if self.gate_text_new_words_threshold == 0 {
            tracing::warn!(
                "gate_text_new_words_threshold=0 disables the text_changed rule"
            );
        }
        if !(0.0..=1.0).contains(&self.text_dedup_similarity) {
            anyhow::bail!("text_dedup_similarity must be in [0, 1]");
        }
        Ok(())
    }
}
