use anyhow::Result;
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use awareness_cli::a11y;
use awareness_cli::aggregator;
use awareness_core::types::{ContextEvent, FilterResponse};
use awareness_cli::audio;
use awareness_cli::backend::Backend;
use awareness_cli::budget::BudgetController;
use awareness_cli::capture;
use awareness_cli::config::{Config, RunArgs};
use awareness_cli::dedup::{PerceptualDedup, TextDedup};
use awareness_cli::eval::{AlertPrompt, spawn_eval_loop};
use awareness_cli::flow::FlowState;
use awareness_cli::memory::{MemoryEntry, MemoryRing};
use awareness_cli::gate::{self, GateAction, GateDecision, GateState};
use awareness_cli::jsonl::JsonlWriter;
use awareness_cli::ocr;
use awareness_cli::tts::TtsConfig;
use awareness_cli::whisper::WhisperEngine;

#[derive(Parser)]
#[command(name = "awareness-cli", version, about = "Awareness POC CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run(RunArgs),
    Analyze {
        #[arg(long, default_value = "data/phase_poc/runs.jsonl")]
        runs: std::path::PathBuf,
        #[arg(long, default_value = "data/phase_poc/ratings.jsonl")]
        ratings: std::path::PathBuf,
    },
}

#[derive(serde::Serialize)]
struct RunLogEntry {
    tick_id: u64,
    timestamp: chrono::DateTime<chrono::Utc>,
    event: ContextEvent,
    gate_decision: GateDecision,
    api_response: Option<FilterResponse>,
    latency_ms: LatencyMs,
}

#[derive(serde::Serialize)]
struct LatencyMs {
    gate: u64,
    api: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run(args) => run(args).await?,
        Commands::Analyze { runs, ratings } => {
            println!(
                "Use scripts/analyze_runs.py for analysis.\nruns={runs:?}\nratings={ratings:?}"
            );
        }
    }

    Ok(())
}

async fn run(args: RunArgs) -> Result<()> {
    let cfg = Config::from_env_and_args(args)?;

    // Ensure output dir exists before we open the log file inside it.
    std::fs::create_dir_all(&cfg.output_dir)?;

    // Dual-sink logging: stdout (pretty, colour) + a run.log file inside the
    // output dir (plain, no ANSI). The file is truncated on every run so it
    // only ever contains the most recent session. Keeps _guard alive for the
    // lifetime of run() so the non-blocking writer flushes on exit.
    let log_path = cfg.output_dir.join("run.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)?;
    let (file_writer, _log_guard) = tracing_appender::non_blocking(log_file);

    // Prefer RUST_LOG if it's set (standard tracing override); fall back to
    // the validated cfg.log_level. Log which source won so debugging filter
    // surprises doesn't require reading source.
    let (filter, filter_source) = match EnvFilter::try_from_default_env() {
        Ok(f) => (f, "RUST_LOG"),
        Err(_) => (EnvFilter::new(&cfg.log_level), "cfg.log_level"),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(fmt::layer().with_writer(file_writer).with_ansi(false))
        .init();

    tracing::info!(
        "awareness-cli starting — budget ${:.2}/day, output {:?}, log {:?}, log_filter_source={}",
        cfg.budget_usd_daily,
        cfg.output_dir,
        log_path,
        filter_source,
    );
    tracing::info!(
        "features: full={}",
        cfg!(feature = "full"),
    );

    // One-time, non-fatal: ensure Chromium/Electron launchers pass
    // --force-renderer-accessibility so AT-SPI sees their content.
    awareness_cli::setup::ensure_a11y_launchers();

    let cfg = Arc::new(cfg);

    // Output dir + JSONL writers
    tokio::fs::create_dir_all(&cfg.output_dir).await?;
    let runs_writer = Arc::new(Mutex::new(
        JsonlWriter::new(cfg.output_dir.join("runs.jsonl")).await?,
    ));
    let ratings_path = cfg.output_dir.join("ratings.jsonl");

    // Budget
    let budget = Arc::new(Mutex::new(BudgetController::new(
        cfg.budget_usd_daily,
        &cfg.output_dir,
    )));

    // Channels
    let (screen_tx, mut screen_rx) = mpsc::channel(4);
    let (mic_tx, mut mic_rx) = mpsc::channel(8);
    let (ocr_tx, ocr_rx) = mpsc::channel(32);
    let (transcript_tx, transcript_rx) = mpsc::channel(32);
    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (alert_tx, alert_rx) = mpsc::channel(16);

    // Capture tasks
    let _screen = capture::spawn_screen_capture(screen_tx, cfg.clone()).await?;
    let _mic = audio::spawn_mic_capture(mic_tx, cfg.clone()).await?;

    // Whisper engine (loaded once, shared via Arc)
    let whisper = Arc::new(WhisperEngine::load(&cfg.whisper_model_path)?);

    // Shared cache for the most recent screenshot (PNG bytes). Only populated
    // when the active backend needs it; the Vision backend reads this at API
    // call time. Updated on every kept frame.
    let latest_png: Arc<Mutex<Option<Arc<Vec<u8>>>>> = Arc::new(Mutex::new(None));

    // Extraction loop: screen_rx → perceptual dedup → try AT-SPI → fall back to
    // OCR → text dedup → ocr_tx. AT-SPI gives us structured, error-free text for
    // native GTK apps. OCR covers the rest. In vision mode, the raw image is
    // also cached for the Vision backend to send to gpt-4o-mini.
    {
        let threshold = cfg.perceptual_hash_threshold;
        let similarity = cfg.text_dedup_similarity;
        let a11y_script = cfg.a11y_script.clone();
        let cache_image = matches!(cfg.backend, awareness_cli::backend::BackendKind::Vision);
        let latest_png = latest_png.clone();
        tokio::spawn(async move {
            let mut pdedup = PerceptualDedup::new(threshold);
            let mut tdedup = TextDedup::new(20, similarity);
            while let Some(frame) = screen_rx.recv().await {
                if !pdedup.should_keep(frame.perceptual_hash) {
                    continue;
                }

                // Cache PNG bytes for the vision backend. Encoding is CPU-bound
                // so we run it in spawn_blocking — but we AWAIT the result
                // before continuing to avoid a race where the gate fires Send
                // before the first PNG is ready (tick 2 bug: "vision backend
                // called without image"). Encoding costs ~100-300 ms per
                // frame, negligible vs the ~1.5 s a11y query that follows.
                if cache_image {
                    let img = frame.image.clone();
                    if let Ok(Some(bytes)) = tokio::task::spawn_blocking(move || {
                        let mut buf = std::io::Cursor::new(Vec::new());
                        img.write_to(&mut buf, image::ImageFormat::Png)
                            .ok()
                            .map(|_| buf.into_inner())
                    })
                    .await
                    {
                        *latest_png.lock().await = Some(Arc::new(bytes));
                    }
                }

                let a11y_out = a11y::try_snapshot(&a11y_script, frame.captured_at, 80, 6)
                    .await
                    .unwrap_or(None);

                let (out, src) = match a11y_out {
                    Some(out) => (out, "a11y"),
                    None => match ocr::extract_text(&frame.image, frame.captured_at) {
                        Ok(out) => (out, "ocr"),
                        Err(e) => {
                            tracing::warn!("OCR: {e}");
                            continue;
                        }
                    },
                };

                tracing::info!(
                    "extract src={src} app={:?} chars={}",
                    out.inferred_app_name,
                    out.full_text.chars().count()
                );

                if tdedup.should_keep(&out.full_text) {
                    let _ = ocr_tx.send(out).await;
                }
            }
        });
    }

    // Whisper loop: mic_rx → transcribe → transcript_tx
    {
        let whisper = whisper.clone();
        tokio::spawn(async move {
            while let Some(chunk) = mic_rx.recv().await {
                let w = whisper.clone();
                let result =
                    tokio::task::spawn_blocking(move || w.transcribe(&chunk)).await;
                match result {
                    Ok(Ok(t)) if !t.text.is_empty() => {
                        let _ = transcript_tx.send(t).await;
                    }
                    Ok(Err(e)) => tracing::warn!("Whisper: {e}"),
                    _ => {}
                }
            }
        });
    }

    // Aggregator: (ocr_rx, transcript_rx) → event_tx
    {
        let cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = aggregator::run(ocr_rx, transcript_rx, event_tx, cfg).await {
                tracing::error!("Aggregator: {e}");
            }
        });
    }

    // Resolve TTS backend once at startup — probes PATH for spd-say/espeak/say.
    let tts_config = TtsConfig::resolve(cfg.tts_enabled, cfg.tts_command.as_deref());
    tracing::info!(
        "tts: enabled={} backend={:?}",
        tts_config.enabled,
        tts_config.command,
    );

    // Eval loop (terminal alerts + ratings + optional TTS)
    let _eval = spawn_eval_loop(alert_rx, ratings_path, tts_config).await?;

    // API backend: text or vision, selected by --backend flag.
    let backend = Backend::new(cfg.backend, &cfg)?;
    tracing::info!("analysis backend: {}", backend.label());

    let memory = Arc::new(Mutex::new(MemoryRing::new(10)));

    // Flow-state detector — suppresses low/medium urgency notifications
    // while the user is deep-focused in an editor/terminal for several minutes.
    let flow_state = Arc::new(Mutex::new(FlowState::new()));

    // Gate + API loop — runs on the main async task
    let mut gate_state = GateState::default();
    let mut tick_id: u64 = 0;
    // Global min-interval cooldown between API sends. Bypassed for the
    // "emotional" gate reason (user wants immediate feedback on frustration).
    let mut last_api_call_at: Option<std::time::Instant> = None;

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                tick_id += 1;

                // Update flow-state tracker on every event (independent of gate outcome).
                flow_state.lock().await.update(&event.app);

                let t0 = std::time::Instant::now();
                let decision = gate::evaluate(&event, &mut gate_state, &cfg);
                let gate_ms = t0.elapsed().as_millis() as u64;

                let mic_preview: Option<String> = event
                    .mic_text_recent
                    .as_deref()
                    .map(|s| s.chars().take(40).collect());
                let screen_preview: String = event
                    .screen_text_excerpt
                    .chars()
                    .take(120)
                    .collect::<String>()
                    .replace('\n', " | ");
                tracing::info!(
                    "tick={tick_id} gate={:?} reason={} app={:?} mic={:?} screen_chars={} screen_preview={:?}",
                    decision.action,
                    decision.reason,
                    event.app,
                    mic_preview,
                    event.screen_text_excerpt.len(),
                    screen_preview,
                );

                let (api_response, api_ms) = if decision.action == GateAction::Send {
                    // Global min-interval cooldown. Emotional events bypass.
                    let cooldown_ok = decision.reason == "emotional"
                        || last_api_call_at.map_or(true, |t| {
                            t.elapsed()
                                >= std::time::Duration::from_secs(cfg.min_send_interval_seconds)
                        });
                    if !cooldown_ok {
                        tracing::debug!(
                            "tick={tick_id} send suppressed by min-interval cooldown ({}s)",
                            cfg.min_send_interval_seconds
                        );
                        (None, None)
                    } else {
                    let b = budget.lock().await;
                        if b.remaining() < 0.0001 {
                            tracing::warn!("Budget exhausted — API disabled");
                            (None, None)
                        } else {
                            drop(b);
                            let t1 = std::time::Instant::now();
                            // Vision backend needs the latest PNG. Clone the
                            // Arc out of the mutex so we don't hold the lock
                            // across the API request.
                            let img_bytes: Option<Arc<Vec<u8>>> = if backend.needs_image() {
                                latest_png.lock().await.clone()
                            } else {
                                None
                            };
                            let img_ref: Option<&[u8]> =
                                img_bytes.as_deref().map(|v| v.as_slice());
                            let mem_str = memory.lock().await.to_prompt_lines();
                            match backend.analyze(&event, img_ref, &mem_str, &decision.reason).await {
                                Ok(resp) => {
                                    let ms = t1.elapsed().as_millis() as u64;
                                    last_api_call_at = Some(std::time::Instant::now());
                                    let mut b = budget.lock().await;
                                    match b.try_spend(resp.cost_usd) {
                                        Ok(_) => {
                                            // If the model's JSON failed to parse, cost was still
                                            // spent (deducted above), but treat the response as
                                            // signal-less: skip alert dispatch and memory update.
                                            if let Some(msg) = &resp.parse_error {
                                                tracing::error!(
                                                    "tick={tick_id} API returned unparseable JSON: {msg}; skipping alert"
                                                );
                                                (Some(resp), Some(ms))
                                            } else {
                                            // Force alert when gate fires emotional — user asked
                                            // for commentary whenever frustration keywords hit,
                                            // regardless of the model's own should_alert verdict.
                                            let force_alert = decision.reason == "emotional";
                                            let mut resp = resp;
                                            if force_alert && !resp.should_alert {
                                                resp.should_alert = true;
                                                if resp.alert_type == "none" {
                                                    resp.alert_type = "emotional".to_string();
                                                }
                                            }
                                            // Guarantee quick_message on alert. Vision/text
                                            // prompts already enforce it, but if the model
                                            // returns empty and we forced the alert, fall back
                                            // to a minimal description derived from local state.
                                            if resp.should_alert && resp.quick_message.trim().is_empty() {
                                                tracing::warn!(
                                                    "tick={tick_id} model returned empty quick_message; using local fallback (reason={})",
                                                    decision.reason
                                                );
                                                resp.quick_message = format!(
                                                    "Sinal local ({}) em app {}.",
                                                    decision.reason,
                                                    event.app.as_deref().unwrap_or("desconhecida"),
                                                );
                                            }
                                            tracing::info!(
                                                "tick={tick_id} alert={} type={} cost=${:.6} left=${:.4} msg={:?}",
                                                resp.should_alert, resp.alert_type,
                                                resp.cost_usd, b.remaining(),
                                                resp.quick_message,
                                            );
                                            memory.lock().await.push(MemoryEntry {
                                                timestamp: chrono::Utc::now(),
                                                app: event.app.clone(),
                                                alert_type: resp.alert_type.clone(),
                                                should_alert: resp.should_alert,
                                                quick_message: resp.quick_message.clone(),
                                            });
                                            if resp.should_alert {
                                                let in_flow = flow_state.lock().await.in_flow();
                                                let suppress = in_flow && resp.urgency != "high";
                                                if suppress {
                                                    tracing::info!(
                                                        "flow_state: suppressing {} urgency alert during focus",
                                                        resp.urgency
                                                    );
                                                } else {
                                                    let _ = alert_tx.try_send(AlertPrompt {
                                                        tick_id,
                                                        event: event.clone(),
                                                        gate_decision: decision.clone(),
                                                        api_response: resp.clone(),
                                                    });
                                                }
                                            }
                                            (Some(resp), Some(ms))
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!("{e}");
                                            println!("\nBudget cap reached. Exiting.");
                                            std::process::exit(2);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("API: {e}");
                                    (None, None)
                                }
                            }
                        }
                    }
                } else {
                    (None, None)
                };

                let entry = RunLogEntry {
                    tick_id,
                    timestamp: event.timestamp,
                    event,
                    gate_decision: decision,
                    api_response,
                    latency_ms: LatencyMs { gate: gate_ms, api: api_ms },
                };
                let w = runs_writer.lock().await;
                if let Err(e) = w.append(&entry).await {
                    tracing::warn!("JSONL write: {e}");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                let b = budget.lock().await;
                println!(
                    "\nShutdown. Ticks: {tick_id} | Cost: ${:.4} | Remaining: ${:.4}",
                    b.spent(), b.remaining()
                );
                break;
            }
        }
    }

    Ok(())
}
