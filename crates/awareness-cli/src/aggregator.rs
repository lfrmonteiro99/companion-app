use crate::config::Config;
use crate::ocr::OcrOutput;
use crate::whisper::TranscriptChunk;
use anyhow::Result;
use chrono::Utc;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub use awareness_core::types::ContextEvent;

/// Max chars sent as screen_text_excerpt. ~2000 tokens — covers most a11y
/// dumps; dense UIs (Teams, Chrome) get truncated and a debug line is logged.
const SCREEN_TEXT_MAX_CHARS: usize = 8000;

/// Cap on the retained app_history. Filtering keeps entries in the last 30
/// minutes, but without a hard cap the deque would grow unbounded for users
/// who app-switch rapidly. 200 is well above realistic switching rates.
const APP_HISTORY_MAX: usize = 200;

#[allow(clippy::too_many_arguments)]
fn build_event(
    current_app: &Option<String>,
    current_window_title: &Option<String>,
    last_screen_text: &str,
    recent_transcripts: &VecDeque<String>,
    app_started_at: &Instant,
    app_history: &VecDeque<(String, u64, Instant)>,
    mic_text_new: bool,
    media_audio_text: &Option<String>,
) -> ContextEvent {
    // 8000 chars (~2000 tokens) covers most a11y dumps (Teams ~20K chars is
    // still truncated but the compose box / visible chat fits). 800 was too
    // tight — only captured app chrome/toolbar, missing actual content.
    let total_chars = last_screen_text.chars().count();
    let screen_text_excerpt = last_screen_text
        .chars()
        .take(SCREEN_TEXT_MAX_CHARS)
        .collect::<String>();
    if total_chars > SCREEN_TEXT_MAX_CHARS {
        tracing::debug!(
            "screen_text truncated: {}/{} chars kept (app={:?})",
            SCREEN_TEXT_MAX_CHARS,
            total_chars,
            current_app
        );
    }

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

    // `Instant - Duration` panics if the result predates the monotonic
    // epoch — reachable whenever system uptime is under 30 minutes. When
    // there's no representable cutoff, every recorded instant is within
    // the window anyway, so keep them all.
    let cutoff = Instant::now().checked_sub(std::time::Duration::from_secs(30 * 60));
    let history_apps_30min = app_history
        .iter()
        .filter(|(_, _, when)| cutoff.is_none_or(|c| *when >= c))
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
        media_audio_text: media_audio_text.clone(),
    }
}

pub async fn run(
    mut ocr_rx: mpsc::Receiver<OcrOutput>,
    mut transcript_rx: mpsc::Receiver<TranscriptChunk>,
    event_tx: mpsc::Sender<ContextEvent>,
    cfg: Arc<Config>,
    mut media_audio_rx: mpsc::Receiver<String>,
) -> Result<()> {
    let mut current_app: Option<String> = None;
    let mut current_window_title: Option<String> = None;
    let mut app_started_at = Instant::now();
    let mut last_screen_text = String::new();
    let mut recent_transcripts: VecDeque<String> = VecDeque::new();
    let mut app_history: VecDeque<(String, u64, Instant)> = VecDeque::new();
    let mut last_media_audio: Option<String> = None;

    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(cfg.tick_analysis_seconds));

    let mut ocr_open = true;
    let mut transcript_open = true;
    let mut media_audio_open = true;

    loop {
        if !ocr_open && !transcript_open && !media_audio_open {
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
                                // Hard cap so the deque can't grow unbounded
                                // over long sessions even though build_event
                                // filters by the 30-min window.
                                while app_history.len() > APP_HISTORY_MAX {
                                    app_history.pop_front();
                                }
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
                                &last_media_audio,
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
                            &last_media_audio,
                        );
                        event_tx.send(event).await?;
                    }
                }
            }

            amsg = media_audio_rx.recv(), if media_audio_open => {
                match amsg {
                    None => { media_audio_open = false; }
                    Some(t) => { last_media_audio = Some(t); }
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
                    &last_media_audio,
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
            openai_api_key: String::new(),
            llm_base_url: crate::config::DEFAULT_LLM_BASE_URL.into(),
            llm_model: crate::config::DEFAULT_LLM_MODEL.into(),
            llm_timeout_seconds: crate::config::DEFAULT_LLM_TIMEOUT_SECONDS,
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
            vision_enabled: false,
            vision_max_image_px: awareness_core::config::DEFAULT_VISION_MAX_IMAGE_PX,
            media_audio_enabled: false,
        })
    }

    fn ocr(app: Option<&str>, title: &str, text: &str) -> OcrOutput {
        OcrOutput {
            captured_at: Utc::now(),
            full_text: text.to_string(),
            title_bar_text: title.to_string(),
            inferred_app_name: app.map(|s| s.to_string()),
            active_bbox: None,
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
            &None,
        );
        assert_eq!(ev.window_title.as_deref(), Some("Doc.md — VSCode"));
        assert_eq!(ev.app.as_deref(), Some("vscode"));
    }

    #[test]
    fn build_event_truncates_huge_screen_text() {
        let big = "a".repeat(SCREEN_TEXT_MAX_CHARS * 3);
        let ev = build_event(
            &Some("app".into()),
            &None,
            &big,
            &VecDeque::new(),
            &Instant::now(),
            &VecDeque::new(),
            false,
            &None,
        );
        assert_eq!(
            ev.screen_text_excerpt.chars().count(),
            SCREEN_TEXT_MAX_CHARS,
            "huge input must be truncated to the cap"
        );
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
            &None,
        );
        assert_eq!(ev.window_title, None);
    }

    #[test]
    fn build_event_carries_media_audio_text() {
        let media = Some("upbeat music playing".to_string());
        let ev = build_event(
            &Some("instagram".into()),
            &None,
            "some text",
            &VecDeque::new(),
            &Instant::now(),
            &VecDeque::new(),
            false,
            &media,
        );
        assert_eq!(ev.media_audio_text.as_deref(), Some("upbeat music playing"));
    }

    #[tokio::test]
    async fn run_populates_window_title_on_app_change() {
        let (ocr_tx, ocr_rx) = mpsc::channel::<OcrOutput>(4);
        let (_transcript_tx, transcript_rx) = mpsc::channel::<crate::whisper::TranscriptChunk>(4);
        let (event_tx, mut event_rx) = mpsc::channel::<ContextEvent>(4);
        let (_media_tx, media_rx) = mpsc::channel::<String>(4);

        let cfg = dummy_cfg();
        let h = tokio::spawn(async move {
            let _ = run(ocr_rx, transcript_rx, event_tx, cfg, media_rx).await;
        });

        // First OCR: app appears → aggregator should emit an immediate event
        // whose window_title reflects the a11y title_bar_text.
        ocr_tx
            .send(ocr(Some("vscode"), "main.rs — VSCode", "fn main()"))
            .await
            .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
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
        let (_media_tx, media_rx) = mpsc::channel::<String>(4);

        let cfg = dummy_cfg();
        let h = tokio::spawn(async move {
            let _ = run(ocr_rx, transcript_rx, event_tx, cfg, media_rx).await;
        });

        ocr_tx
            .send(ocr(Some("app1"), "   ", "body text"))
            .await
            .unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("aggregator must emit within 1s")
            .expect("event closed");
        assert_eq!(
            ev.window_title, None,
            "whitespace-only title must map to None"
        );

        // The aggregator loop only exits once both senders are dropped; we
        // got what we asserted so abort instead of blocking forever on the
        // periodic-tick branch (3600s cfg).
        h.abort();
    }

    #[tokio::test]
    async fn run_media_audio_text_appears_on_next_event() {
        let (ocr_tx, ocr_rx) = mpsc::channel::<OcrOutput>(4);
        let (_transcript_tx, transcript_rx) = mpsc::channel::<crate::whisper::TranscriptChunk>(4);
        let (event_tx, mut event_rx) = mpsc::channel::<ContextEvent>(8);
        let (media_tx, media_rx) = mpsc::channel::<String>(4);

        let cfg = dummy_cfg();
        let h = tokio::spawn(async move {
            let _ = run(ocr_rx, transcript_rx, event_tx, cfg, media_rx).await;
        });

        // Send a media audio transcript first.
        media_tx.send("artist singing a pop song".to_string()).await.unwrap();

        // Give the aggregator a tick to process the media audio message before the OCR arrives.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Trigger an immediate event via app change on OCR.
        ocr_tx
            .send(ocr(Some("instagram"), "Instagram Reels", "reel content"))
            .await
            .unwrap();

        // The next emitted event should carry the media_audio_text.
        let ev = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("aggregator must emit within 1s")
            .expect("event channel closed");
        assert_eq!(
            ev.media_audio_text.as_deref(),
            Some("artist singing a pop song"),
            "media_audio_text must be carried on next event after media channel message"
        );

        h.abort();
    }
}
