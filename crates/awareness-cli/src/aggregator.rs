use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use crate::config::Config;
use crate::ocr::OcrOutput;
use crate::whisper::TranscriptChunk;

/// The central event type. Passed to the gate, then API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEvent {
    pub timestamp: DateTime<Utc>,
    pub app: Option<String>,
    pub window_title: Option<String>,
    pub screen_text_excerpt: String,
    pub mic_text_recent: Option<String>,
    pub duration_on_app_seconds: u64,
    pub history_apps_30min: Vec<(String, u64)>,
}

fn build_event(
    current_app: &Option<String>,
    last_screen_text: &str,
    recent_transcripts: &VecDeque<String>,
    app_started_at: &Instant,
    app_history: &VecDeque<(String, u64, Instant)>,
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
        window_title: None,
        screen_text_excerpt,
        mic_text_recent,
        duration_on_app_seconds,
        history_apps_30min,
    }
}

pub async fn run(
    mut ocr_rx: mpsc::Receiver<OcrOutput>,
    mut transcript_rx: mpsc::Receiver<TranscriptChunk>,
    event_tx: mpsc::Sender<ContextEvent>,
    cfg: Arc<Config>,
) -> Result<()> {
    let mut current_app: Option<String> = None;
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
                                &last_screen_text,
                                &recent_transcripts,
                                &app_started_at,
                                &app_history,
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
                        if recent_transcripts.len() == 3 {
                            recent_transcripts.pop_front();
                        }
                        recent_transcripts.push_back(chunk.text);
                    }
                }
            }

            _ = interval.tick() => {
                let event = build_event(
                    &current_app,
                    &last_screen_text,
                    &recent_transcripts,
                    &app_started_at,
                    &app_history,
                );
                event_tx.send(event).await?;
            }
        }
    }
}
