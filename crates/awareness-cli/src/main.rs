use anyhow::Result;
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use awareness_cli::a11y;
use awareness_cli::aggregator;
use awareness_cli::audio;
use awareness_cli::backend::Backend;
use awareness_cli::budget::BudgetController;
use awareness_cli::capture;
use awareness_cli::config::{Config, RunArgs};
use awareness_cli::dedup::{PerceptualDedup, TextDedup};
use awareness_cli::eval::{spawn_eval_loop, AlertPrompt};
use awareness_cli::flow::FlowState;
use awareness_cli::gate::{self, GateAction, GateDecision, GateState};
use awareness_cli::jsonl::JsonlWriter;
use awareness_cli::memory::{MemoryEntry, MemoryRing};
use awareness_cli::ocr;
use awareness_cli::tts::TtsConfig;
use awareness_cli::whisper::WhisperEngine;
use awareness_core::types::{ContextEvent, FilterResponse};

#[derive(Parser)]
#[command(name = "awareness-cli", version, about = "Awareness POC CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run(Box<RunArgs>),
    /// Compute go/no-go metrics from a directory of JSONL run logs.
    /// Thin wrapper over scripts/analyze_runs.py (keeps the heavy stats in
    /// Python so the Rust dep tree doesn't balloon).
    Analyze {
        /// Directory of JSONL run files. Default matches the Run default
        /// output_dir.
        #[arg(long, default_value = "data/phase_poc")]
        runs_dir: std::path::PathBuf,
        /// Optional markdown report output path.
        #[arg(long)]
        output_md: Option<std::path::PathBuf>,
        /// Path to the analysis script. Searched as given, and relative to
        /// the repo root layout (../../scripts/analyze_runs.py).
        #[arg(long, default_value = "scripts/analyze_runs.py")]
        script: std::path::PathBuf,
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
        Commands::Run(args) => run(*args).await?,
        Commands::Analyze {
            runs_dir,
            output_md,
            script,
        } => {
            analyze(runs_dir, output_md, script)?;
        }
    }

    Ok(())
}

/// Crop the resized `ScreenFrame.image` (typically 1280x720) down to the
/// focused window using the a11y-reported bounding box (in native screen
/// pixels). Returns `None` when:
///   - we have no bbox (a11y didn't expose Component),
///   - the frame is window-only already (sidecar capture),
///   - the bbox shrinks to near-zero after clamping (off-screen window).
fn crop_to_active_window(
    resized: &image::DynamicImage,
    native_size: Option<(u32, u32)>,
    bbox: Option<(i32, i32, u32, u32)>,
) -> Option<image::DynamicImage> {
    let (nat_w, nat_h) = native_size?;
    let (bx, by, bw, bh) = bbox?;
    if nat_w == 0 || nat_h == 0 {
        return None;
    }
    let img_w = resized.width();
    let img_h = resized.height();
    let sx = img_w as f32 / nat_w as f32;
    let sy = img_h as f32 / nat_h as f32;
    let cx = ((bx.max(0) as f32) * sx) as u32;
    let cy = ((by.max(0) as f32) * sy) as u32;
    let cw = ((bw as f32) * sx) as u32;
    let ch = ((bh as f32) * sy) as u32;
    let cx = cx.min(img_w);
    let cy = cy.min(img_h);
    let cw = cw.min(img_w.saturating_sub(cx));
    let ch = ch.min(img_h.saturating_sub(cy));
    // A window crop smaller than a thumbnail is never useful input for
    // OCR/vision; bail out and let the full frame through.
    if cw < 64 || ch < 64 {
        return None;
    }
    Some(resized.crop_imm(cx, cy, cw, ch))
}

/// Resolve the analysis script to an existing path. Tries the given path,
/// then the usual repo layouts (running from the crate dir vs. workspace
/// root). Returns an error if none exist.
fn resolve_script(script: &std::path::Path) -> Result<std::path::PathBuf> {
    let candidates: &[std::path::PathBuf] = &[
        script.to_path_buf(),
        std::path::PathBuf::from("..").join("..").join(script),
    ];
    for c in candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "analysis script not found. Tried: {}",
        candidates
            .iter()
            .map(|p| format!("{:?}", p))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn analyze(
    runs_dir: std::path::PathBuf,
    output_md: Option<std::path::PathBuf>,
    script: std::path::PathBuf,
) -> Result<()> {
    let script_path = resolve_script(&script)?;
    if !runs_dir.exists() {
        anyhow::bail!("runs_dir {:?} does not exist", runs_dir);
    }

    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script_path).arg("--runs-dir").arg(&runs_dir);
    if let Some(md) = output_md.as_deref() {
        cmd.arg("--output-md").arg(md);
    }

    let status = cmd.status().map_err(|e| {
        anyhow::anyhow!("failed to launch python3 (install python3 or pass --script): {e}")
    })?;

    if !status.success() {
        anyhow::bail!("analyze script exited with {status}");
    }
    Ok(())
}

/// Handle a single `GateAction::Send` decision: budget reserve → API call →
/// commit/refund → alert dispatch. Factored out of `run()` so the main loop
/// body stays shallow. Returns `(api_response, api_latency_ms)` for the
/// JSONL log entry.
#[allow(clippy::too_many_arguments)]
async fn process_send(
    tick_id: u64,
    event: &ContextEvent,
    decision: &GateDecision,
    backend: &Backend,
    budget: &Arc<Mutex<BudgetController>>,
    latest_png: &Arc<Mutex<Option<Arc<Vec<u8>>>>>,
    memory: &Arc<Mutex<MemoryRing>>,
    flow_state: &Arc<Mutex<FlowState>>,
    alert_tx: &tokio::sync::mpsc::Sender<AlertPrompt>,
    cfg: &Config,
    last_api_call_at: &mut Option<std::time::Instant>,
) -> (Option<FilterResponse>, Option<u64>) {
    // Global min-interval cooldown. Emotional events bypass.
    let cooldown_ok = decision.reason == "emotional"
        || last_api_call_at.is_none_or(|t| {
            t.elapsed() >= std::time::Duration::from_secs(cfg.min_send_interval_seconds)
        });
    if !cooldown_ok {
        tracing::debug!(
            "tick={tick_id} send suppressed by min-interval cooldown ({}s)",
            cfg.min_send_interval_seconds
        );
        return (None, None);
    }

    // Race-safe budget gate: reserve a conservative upper bound BEFORE the
    // API call, commit/refund afterwards. Concurrent ticks can't both pass
    // because the first reservation is already charged against spent_usd.
    let reservation = {
        let mut b = budget.lock().await;
        match b.try_reserve(backend.max_cost_estimate_usd()) {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!("Budget exhausted — API disabled");
                return (None, None);
            }
        }
    };

    let t1 = std::time::Instant::now();
    let img_bytes: Option<Arc<Vec<u8>>> = if backend.needs_image() {
        latest_png.lock().await.clone()
    } else {
        None
    };
    let img_ref: Option<&[u8]> = img_bytes.as_deref().map(|v| v.as_slice());
    let mem_str = memory.lock().await.to_prompt_lines();

    let resp = match backend
        .analyze(event, img_ref, &mem_str, &decision.reason, "")
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("API: {e}");
            // Return the reservation — the call never happened, so the
            // reserved budget must be released.
            budget.lock().await.refund(reservation);
            return (None, None);
        }
    };

    let ms = t1.elapsed().as_millis() as u64;
    *last_api_call_at = Some(std::time::Instant::now());

    // Reconcile the reservation with the real cost.
    {
        let mut b = budget.lock().await;
        b.commit(reservation, resp.cost_usd);
        if b.spent() > cfg.budget_usd_daily {
            // Actual cost pushed us past the limit — shouldn't happen with
            // a conservative estimate, but if it does, exit cleanly after
            // this alert is dispatched (the user just paid for it).
            tracing::error!(
                "Daily budget exceeded after commit: ${:.4} of ${:.4} — exiting",
                b.spent(),
                cfg.budget_usd_daily
            );
            b.flush();
            println!("\nBudget cap reached. Exiting.");
            std::process::exit(2);
        }
    }

    // If the model's JSON failed to parse, cost was still spent but the
    // response carries no signal — log, skip alert dispatch and memory.
    if let Some(msg) = &resp.parse_error {
        tracing::error!("tick={tick_id} API returned unparseable JSON: {msg}; skipping alert");
        return (Some(resp), Some(ms));
    }

    // Force alert ONLY when the user's intent is unambiguous and the gate
    // rule itself carries signal the model can't silently discard:
    //   - `emotional`: frustration keyword in screen/mic → user wants commentary.
    //   - `voice_activity`: user spoke → expects a reply.
    // For every other gate reason we trust the model's `should_alert`
    // verdict. Previously we forced delivery for all rules and ended up
    // spamming "Ecrã mostra várias aplicações..." fallback messages the
    // model emitted precisely because it decided there was nothing useful
    // to say.
    let force_alert = matches!(decision.reason.as_str(), "emotional" | "voice_activity");
    let mut resp = resp;
    if force_alert && !resp.should_alert {
        resp.should_alert = true;
        if resp.alert_type == "none" {
            resp.alert_type = decision.reason.clone();
        }
    }
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

    {
        let b = budget.lock().await;
        tracing::info!(
            "tick={tick_id} alert={} type={} cost=${:.6} left=${:.4} msg={:?}",
            resp.should_alert,
            resp.alert_type,
            resp.cost_usd,
            b.remaining(),
            resp.quick_message,
        );
    }

    // Only remember alerts that actually fired. Stashing every
    // should_alert=false response floods the ring with the model's
    // low-signal fallback phrases ("Ecrã mostra várias aplicações..."),
    // and the next call's prompt passes those back in as "Histórico
    // recente" — the model then quotes them into its next quick_message
    // instead of reading the current screen. The prompt already forbids
    // citing memory, but curbing the source is more reliable than
    // asking the model not to.
    if resp.should_alert {
        let trimmed: String = resp
            .quick_message
            .chars()
            .take(80)
            .collect::<String>();
        memory.lock().await.push(MemoryEntry {
            timestamp: chrono::Utc::now(),
            app: event.app.clone(),
            alert_type: resp.alert_type.clone(),
            should_alert: true,
            quick_message: trimmed,
        });
    }

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
    tracing::info!("features: full={}", cfg!(feature = "full"),);

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

                // Query a11y FIRST so we can crop the frame to the active
                // window's bounding box before encoding the PNG / running
                // OCR. Three outcomes:
                //   Rich → structured text, skip OCR.
                //   Thin → we at least have the active window's bbox (VS
                //          Code Monaco, Electron canvas, ...) → crop and
                //          run OCR on the focused region.
                //   None → full-frame OCR as last resort.
                let a11y_result = a11y::try_snapshot(&a11y_script, frame.captured_at, 80, 6)
                    .await
                    .unwrap_or(a11y::A11yResult::None);

                let bbox = match &a11y_result {
                    a11y::A11yResult::Rich(o) => o.active_bbox,
                    a11y::A11yResult::Thin(h) => h.active_bbox,
                    a11y::A11yResult::None => None,
                };
                let focused_image =
                    crop_to_active_window(&frame.image, frame.native_size, bbox);

                // Cache PNG bytes for the vision backend. Encoded from the
                // focused image when available so gpt-4o-mini vision sees
                // the window instead of desktop confetti.
                if cache_image {
                    let img = focused_image
                        .clone()
                        .unwrap_or_else(|| frame.image.clone());
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

                let (out, src) = match a11y_result {
                    a11y::A11yResult::Rich(out) => (out, "a11y"),
                    other => {
                        // Thin or None — OCR the cropped region when we
                        // have one. In the Thin case, inherit app/title
                        // from the hint so the event carries the correct
                        // window identity even though OCR generated the
                        // text.
                        let ocr_input = focused_image.as_ref().unwrap_or(&frame.image);
                        match ocr::extract_text(ocr_input, frame.captured_at) {
                            Ok(mut out) => {
                                if let a11y::A11yResult::Thin(hint) = other {
                                    out.inferred_app_name = hint
                                        .inferred_app_name
                                        .or(out.inferred_app_name);
                                    if !hint.title.is_empty() {
                                        out.title_bar_text = hint.title;
                                    }
                                    out.active_bbox = out.active_bbox.or(bbox);
                                }
                                (out, "ocr")
                            }
                            Err(e) => {
                                tracing::warn!("OCR: {e}");
                                continue;
                            }
                        }
                    }
                };

                tracing::info!(
                    "extract src={src} app={:?} chars={} cropped={}",
                    out.inferred_app_name,
                    out.full_text.chars().count(),
                    focused_image.is_some(),
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
                let result = tokio::task::spawn_blocking(move || w.transcribe(&chunk)).await;
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

    // Capacity 4: enough history to detect "same thing still visible, don't
    // re-alert", small enough that the model can't lean on it as a substitute
    // for reading the current frame.
    let memory = Arc::new(Mutex::new(MemoryRing::new(4)));

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
                    process_send(
                        tick_id, &event, &decision, &backend, &budget,
                        &latest_png, &memory, &flow_state, &alert_tx,
                        &cfg, &mut last_api_call_at,
                    ).await
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
                // Drain any pending write-batch so mid-batch spends aren't
                // lost to a crash between now and the next natural flush.
                let mut b = budget.lock().await;
                b.flush();
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
