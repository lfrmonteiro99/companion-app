//! End-to-end smoke test against the configured local LLM.
//!
//! Runs one synthetic Teams-ping tick through `OpenAiClient::filter_call`
//! and prints latency, JSON validity, and the resulting `quick_message`
//! so the desktop pipeline can be validated against OMEN/Ollama without
//! booting the full CLI (which requires audio, OCR, and a11y).
//!
//! Usage:
//!   cargo run -p awareness-core --example smoke_llm
//!   AWARENESS_LLM_MODEL=aya:8b cargo run -p awareness-core --example smoke_llm

use awareness_core::api::OpenAiClient;
use awareness_core::config::{
    Config, DEFAULT_LLM_BASE_URL, DEFAULT_LLM_MODEL, DEFAULT_LLM_TIMEOUT_SECONDS,
};
use awareness_core::types::ContextEvent;
use chrono::Utc;

fn cfg() -> Config {
    let base_url = std::env::var("AWARENESS_LLM_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_LLM_BASE_URL.into());
    let model =
        std::env::var("AWARENESS_LLM_MODEL").unwrap_or_else(|_| DEFAULT_LLM_MODEL.into());
    Config {
        openai_api_key: String::new(),
        llm_base_url: base_url,
        llm_model: model,
        llm_timeout_seconds: DEFAULT_LLM_TIMEOUT_SECONDS,
        budget_usd_daily: 0.0,
        tick_screen_seconds: 2,
        tick_analysis_seconds: 10,
        whisper_model_path: std::path::PathBuf::new(),
        perceptual_hash_threshold: 3,
        text_dedup_similarity: 0.85,
        gate_app_time_threshold_minutes: 25,
        gate_periodic_check_minutes: 2,
        gate_text_new_words_threshold: 5,
        gate_text_change_cooldown_seconds: 6,
        gate_voice_cooldown_seconds: 5,
        gate_frustration_keywords: vec![],
        min_send_interval_seconds: 15,
        transcript_window_size: 5,
        tts_enabled: false,
        tts_command: None,
        output_dir: std::path::PathBuf::new(),
        log_level: "info".into(),
        a11y_script: std::path::PathBuf::new(),
        backend: awareness_core::backend::BackendKind::Text,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cfg = cfg();
    println!(
        "endpoint={} model={} timeout={}s",
        cfg.llm_base_url, cfg.llm_model, cfg.llm_timeout_seconds
    );

    let client = OpenAiClient::new(&cfg)?;

    // SCENARIO=teams (default) | instagram_empty | instagram_match
    let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "teams".into());
    let (event, matched_interests, profile_ctx): (ContextEvent, Vec<String>, &str) =
        match scenario.as_str() {
            "instagram_empty" => (
                ContextEvent {
                    timestamp: Utc::now(),
                    app: Some("Instagram".into()),
                    window_title: Some("Instagram — Feed".into()),
                    screen_text_excerpt:
                        "fitness_pro: '5 dicas para hipertrofia em 2026'. Like 1.2k Comments 38\n\
                         travel.daily: 'Tóquio em Abril: cherry blossoms guide'. Like 8.7k\n\
                         memes_pt: 'quando o café acaba ao domingo'. Like 23k Comments 412"
                            .into(),
                    mic_text_recent: None,
                    duration_on_app_seconds: 120,
                    history_apps_30min: vec![("Instagram".into(), 120)],
                    mic_text_new: false,
                },
                vec![],
                "",
            ),
            "instagram_match" => (
                ContextEvent {
                    timestamp: Utc::now(),
                    app: Some("Instagram".into()),
                    window_title: Some("Instagram — Feed".into()),
                    screen_text_excerpt:
                        "rust_lang: 'async/await internals: state machines and the Pin marker'. \
                         Like 4.1k Comments 187. 'Future requires Pin precisely because the \
                         compiler-generated state machine self-references its locals.'"
                            .into(),
                    mic_text_recent: None,
                    duration_on_app_seconds: 120,
                    history_apps_30min: vec![("Instagram".into(), 120)],
                    mic_text_new: false,
                },
                vec!["Rust async".into()],
                "Sobre o utilizador: dev Rust focado em sistemas distribuídos.\nInteresses confirmados pelo utilizador: Rust async",
            ),
            _ => (
                ContextEvent {
                    timestamp: Utc::now(),
                    app: Some("Teams".into()),
                    window_title: Some("Microsoft Teams - Chat".into()),
                    screen_text_excerpt:
                        "João Silva [há 9 min]: 'PR #142 pronto? quero fazer merge antes da release das 18h'\nVocê: (a escrever...)"
                            .into(),
                    mic_text_recent: None,
                    duration_on_app_seconds: 45,
                    history_apps_30min: vec![],
                    mic_text_new: false,
                },
                vec![],
                "",
            ),
        };
    println!("scenario={} matched_interests={:?}", scenario, matched_interests);

    let t0 = std::time::Instant::now();
    let resp = client
        .filter_call(&event, "", profile_ctx, &matched_interests)
        .await?;
    let elapsed = t0.elapsed();

    println!(
        "elapsed={:.2}s tokens_in={} tokens_out={} parse_error={:?}",
        elapsed.as_secs_f64(),
        resp.tokens_in,
        resp.tokens_out,
        resp.parse_error
    );
    println!(
        "should_alert={} alert_type={} urgency={}",
        resp.should_alert, resp.alert_type, resp.urgency
    );
    println!("quick_message: {}", resp.quick_message);

    // Sanity checks the user reads at a glance.
    if resp.parse_error.is_some() {
        anyhow::bail!("LLM returned unparseable JSON");
    }
    Ok(())
}
