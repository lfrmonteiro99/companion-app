use anyhow::Result;
use chrono::Utc;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use crate::config::Config;
use crate::ocr::OcrOutput;
use crate::whisper::TranscriptChunk;

pub use awareness_core::types::ContextEvent;

fn build_event(
    current_app: &Option<String>,
    current_window_title: &Option<String>,
    last_screen_text: &str,
    recent_transcripts: &VecDeque<String>,
    app_started_at: &Instant,
    app_history: &VecDeque<(String, u64, Instant)>,
    mic_text_new: bool,
) -> ContextEvent {
    // 8000 chars (~2000 tokens) covers most a11y dumps (Teams ~20K chars is
    // still truncated but the compose box / visible chat fits). 800 was too
    // tight — only captured app chrome/toolbar, missing actual content.
    let screen_text_excerpt = last_screen_text.chars().take(8000).collect::<String>();

    let mic_text_recent = if recent_transcripts.is_empty() {
        None
    } else {
        let joined = recent_transcripts
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        Some(joined)
    };

    let duration_on_app_seconds = app_started_at.elapsed().as_secs();

    let thirty_min_ago = Instant::now() - std::time::Duration::from_secs(30 * 60);
    let history_apps_30min = app_history
        .iter()
        .filter(|(_, _, when)| *when >= thirty_min_ago)
        .map(|(app, secs, _)| (app.clone(), *secs))
        .collect();

    ContextEvent {
        timestamp: Utc::now(),
        app: current_app.clone(),
        window_title: current_window_title.clone(),
        screen_text_excerpt,
        mic_text_recent,
        duration_on_app_seconds,
        history_apps_30min,
        mic_text_new,
    }
}

pub async fn run(
    mut ocr_rx: mpsc::Receiver<OcrOutput>,
    mut transcript_rx: mpsc::Receiver<TranscriptChunk>,
    event_tx: mpsc::Sender<ContextEvent>,
    cfg: Arc<Config>,
) -> Result<()> {
    let mut current_app: Option<String> = None;
    let mut current_window_title: Option<String> = None;
    let mut app_started_at = Instant::now();
    let mut last_screen_text = String::new();
    let mut recent_transcripts: VecDeque<String> = VecDeque::new();
    let mut app_history: VecDeque<(String, u64, Instant)> = VecDeque::new();

    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(cfg.tick_analysis_seconds));

    let mut ocr_open = true;
    let mut transcript_open = true;

    loop {
        if !ocr_open && !transcript_open {
            return Ok(());
        }

        tokio::select! {
            ocr_msg = ocr_rx.recv(), if ocr_open => {
                match ocr_msg {
                    None => { ocr_open = false; }
                    Some(ocr) => {
                        last_screen_text = ocr.full_text.clone();
                        // title_bar_text comes from either a11y (window title)
                        // or OCR of the top strip. Treat empty as unknown.
                        current_window_title = if ocr.title_bar_text.trim().is_empty() {
                            None
                        } else {
                            Some(ocr.title_bar_text.trim().to_string())
                        };

                        let new_app = ocr.inferred_app_name.clone();
                        let app_changed = new_app != current_app;

                        if app_changed {
                            // Push old app to history with elapsed seconds.
                            if let Some(ref old_app) = current_app {
                                let elapsed = app_started_at.elapsed().as_secs();
                                app_history.push_back((old_app.clone(), elapsed, app_started_at));
                            }
                            current_app = new_app;
                            app_started_at = Instant::now();

                            // App change is important — emit immediately.
                            let event = build_event(
                                &current_app,
                                &current_window_title,
                                &last_screen_text,
                                &recent_transcripts,
                                &app_started_at,
                                &app_history,
                                false,
                            );
                            event_tx.send(event).await?;
                        } else {
                            current_app = new_app;
                        }
                    }
                }
            }

            transcript_msg = transcript_rx.recv(), if transcript_open => {
                match transcript_msg {
                    None => { transcript_open = false; }
                    Some(chunk) => {
                        if recent_transcripts.len() >= cfg.transcript_window_size.max(1) {
                            recent_transcripts.pop_front();
                        }
                        recent_transcripts.push_back(chunk.text);

                        // Fresh speech reached us — emit an event immediately so
                        // the gate's voice_activity rule can decide whether to
                        // send without waiting for the next 10s periodic tick.
                        let event = build_event(
                            &current_app,
                            &current_window_title,
                            &last_screen_text,
                            &recent_transcripts,
                            &app_started_at,
                            &app_history,
                            true,
                        );
                        event_tx.send(event).await?;
                    }
                }
            }

            _ = interval.tick() => {
                let event = build_event(
                    &current_app,
                    &current_window_title,
                    &last_screen_text,
                    &recent_transcripts,
                    &app_started_at,
                    &app_history,
                    false,
                );
                event_tx.send(event).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendKind;

    fn dummy_cfg() -> Arc<Config> {
        Arc::new(Config {
            openai_api_key: "test".into(),
            budget_usd_daily: 1.0,
            tick_screen_seconds: 2,
            // Long interval so the periodic-tick branch doesn't emit during
            // the short lifetime of these tests.
            tick_analysis_seconds: 3600,
            whisper_model_path: std::path::PathBuf::from("m.bin"),
            perceptual_hash_threshold: 3,
            text_dedup_similarity: 0.99,
            gate_app_time_threshold_minutes: 25,
            gate_periodic_check_minutes: 2,
            gate_text_new_words_threshold: 5,
            gate_text_change_cooldown_seconds: 6,
            gate_voice_cooldown_seconds: 5,
            gate_frustration_keywords: crate::config_file::default_frustration_keywords(),
            min_send_interval_seconds: 15,
            transcript_window_size: 5,
            tts_enabled: false,
            tts_command: None,
            output_dir: std::path::PathBuf::from("data"),
            log_level: "info".into(),
            a11y_script: std::path::PathBuf::from("a11y.py"),
            backend: BackendKind::Text,
        })
    }

    fn ocr(app: Option<&str>, title: &str, text: &str) -> OcrOutput {
        OcrOutput {
            captured_at: Utc::now(),
            full_text: text.to_string(),
            title_bar_text: title.to_string(),
            inferred_app_name: app.map(|s| s.to_string()),
        }
    }

    #[test]
    fn build_event_carries_window_title() {
        let title = Some("Doc.md — VSCode".to_string());
        let ev = build_event(
            &Some("vscode".into()),
            &title,
            "hello world",
            &VecDeque::new(),
            &Instant::now(),
            &VecDeque::new(),
            false,
        );
        assert_eq!(ev.window_title.as_deref(), Some("Doc.md — VSCode"));
        assert_eq!(ev.app.as_deref(), Some("vscode"));
    }

    #[test]
    fn build_event_passes_none_title_through() {
        let ev = build_event(
            &Some("vscode".into()),
            &None,
            "hello",
            &VecDeque::new(),
            &Instant::now(),
            &VecDeque::new(),
            false,
        );
        assert_eq!(ev.window_title, None);
    }

    #[tokio::test]
    async fn run_populates_window_title_on_app_change() {
        let (ocr_tx, ocr_rx) = mpsc::channel::<OcrOutput>(4);
        let (_transcript_tx, transcript_rx) = mpsc::channel::<crate::whisper::TranscriptChunk>(4);
        let (event_tx, mut event_rx) = mpsc::channel::<ContextEvent>(4);

        let cfg = dummy_cfg();
        let h = tokio::spawn(async move {
            let _ = run(ocr_rx, transcript_rx, event_tx, cfg).await;
        });

        // First OCR: app appears → aggregator should emit an immediate event
        // whose window_title reflects the a11y title_bar_text.
        ocr_tx
            .send(ocr(Some("vscode"), "main.rs — VSCode", "fn main()"))
            .await
            .unwrap();
        let ev = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            event_rx.recv(),
        )
        .await
        .expect("aggregator must emit within 1s")
        .expect("event channel closed unexpectedly");
        assert_eq!(ev.window_title.as_deref(), Some("main.rs — VSCode"));
        assert_eq!(ev.app.as_deref(), Some("vscode"));

        // The aggregator loop only exits once both senders are dropped; we
        // got what we asserted so abort instead of blocking forever on the
        // periodic-tick branch (3600s cfg).
        h.abort();
    }

    #[tokio::test]
    async fn run_treats_empty_title_as_none() {
        let (ocr_tx, ocr_rx) = mpsc::channel::<OcrOutput>(4);
        let (_transcript_tx, transcript_rx) = mpsc::channel::<crate::whisper::TranscriptChunk>(4);
        let (event_tx, mut event_rx) = mpsc::channel::<ContextEvent>(4);

        let cfg = dummy_cfg();
        let h = tokio::spawn(async move {
            let _ = run(ocr_rx, transcript_rx, event_tx, cfg).await;
        });

        ocr_tx
            .send(ocr(Some("app1"), "   ", "body text"))
            .await
            .unwrap();
        let ev = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            event_rx.recv(),
        )
        .await
        .expect("aggregator must emit within 1s")
        .expect("event closed");
        assert_eq!(ev.window_title, None, "whitespace-only title must map to None");

        // The aggregator loop only exits once both senders are dropped; we
        // got what we asserted so abort instead of blocking forever on the
        // periodic-tick branch (3600s cfg).
        h.abort();
    }
}
