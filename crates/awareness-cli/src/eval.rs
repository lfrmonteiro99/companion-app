use anyhow::Result;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use crate::aggregator::ContextEvent;
use crate::api::FilterResponse;
use crate::gate::GateDecision;

// ── Public types ──────────────────────────────────────────────────────────────

/// A gated event + API response, ready for display.
#[derive(Debug, Clone)]
pub struct AlertPrompt {
    pub tick_id: u64,
    pub event: ContextEvent,
    pub gate_decision: GateDecision,
    pub api_response: FilterResponse,
}

/// A user rating for an alert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rating {
    pub tick_id: u64,
    pub rating: String, // "useful" | "not_useful" | "annoying" | "skipped" | "timeout" | "error"
    pub note: Option<String>,
}

/// One stdin read attempt resolves to one of these.
enum PromptOutcome {
    Input(String),
    Timeout,
    Eof,
    IoError(String),
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Spawns the eval display loop.
/// Receives AlertPrompts, displays them, collects ratings, writes to ratings_path.
///
/// A dedicated OS thread owns stdin so read_line never blocks the tokio runtime
/// and buffered input survives across prompts.
pub async fn spawn_eval_loop(
    mut alert_rx: mpsc::Receiver<AlertPrompt>,
    ratings_path: PathBuf,
) -> Result<JoinHandle<()>> {
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<PromptOutcome>();

    std::thread::Builder::new()
        .name("eval-stdin".into())
        .spawn(move || {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            let mut buf = String::new();
            loop {
                buf.clear();
                match stdin.lock().read_line(&mut buf) {
                    Ok(0) => {
                        let _ = line_tx.send(PromptOutcome::Eof);
                        break;
                    }
                    Ok(_) => {
                        if line_tx.send(PromptOutcome::Input(buf.clone())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = line_tx.send(PromptOutcome::IoError(e.to_string()));
                        break;
                    }
                }
            }
        })?;

    let fallback_path = ratings_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("ratings_failed.jsonl");

    let handle = tokio::spawn(async move {
        while let Some(prompt) = alert_rx.recv().await {
            handle_prompt(&prompt, &ratings_path, &fallback_path, &mut line_rx).await;
        }
    });

    Ok(handle)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

async fn handle_prompt(
    prompt: &AlertPrompt,
    ratings_path: &PathBuf,
    fallback_path: &PathBuf,
    line_rx: &mut mpsc::UnboundedReceiver<PromptOutcome>,
) {
    let now = Local::now().format("%H:%M").to_string();
    let alert_type_upper = prompt.api_response.alert_type.to_uppercase();
    let quick_message = &prompt.api_response.quick_message;

    // Bell + coloured alert line
    print!("\x07");
    println!(
        "\n\x1b[93m[{}] {}: {}\x1b[0m",
        now, alert_type_upper, quick_message
    );
    println!("Rating [u=useful / n=not_useful / a=annoying / <enter>=skip]:");

    let urgency = match prompt.api_response.urgency.as_str() {
        "high" => "critical",
        "low"  => "low",
        _      => "normal",
    };
    let notify_title = format!("Awareness — {}", alert_type_upper);
    let _ = tokio::process::Command::new("notify-send")
        .args([
            "--app-name=awareness-cli",
            "--icon=dialog-information",
            "--expire-time=15000",
            &format!("--urgency={urgency}"),
            &notify_title,
            quick_message,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    let outcome = next_outcome(line_rx, 30).await;
    let rating_str: &str = match &outcome {
        PromptOutcome::Input(s) => match s.trim() {
            "u" => "useful",
            "n" => "not_useful",
            "a" => "annoying",
            _ => "skipped",
        },
        PromptOutcome::Timeout => "timeout",
        PromptOutcome::Eof => "skipped",
        PromptOutcome::IoError(msg) => {
            tracing::error!("eval stdin read error: {msg}");
            "error"
        }
    };

    let rating = Rating {
        tick_id: prompt.tick_id,
        rating: rating_str.to_string(),
        note: None,
    };

    if let Err(e) = append_rating(ratings_path, &rating).await {
        tracing::error!("Failed to write rating to {:?}: {:?}", ratings_path, e);
        if let Err(e2) = append_rating(fallback_path, &rating).await {
            tracing::error!(
                "Also failed fallback write to {:?}: {:?}. Raw line: {:?}",
                fallback_path, e2, rating,
            );
        }
    }
}

/// Wait for the next stdin outcome, or time out after `timeout_secs`.
async fn next_outcome(
    line_rx: &mut mpsc::UnboundedReceiver<PromptOutcome>,
    timeout_secs: u64,
) -> PromptOutcome {
    match tokio::time::timeout(Duration::from_secs(timeout_secs), line_rx.recv()).await {
        Ok(Some(o)) => o,
        Ok(None) => PromptOutcome::Eof,
        Err(_) => PromptOutcome::Timeout,
    }
}

/// Append a Rating as a JSONL line to the given file (create if missing).
async fn append_rating(path: &PathBuf, rating: &Rating) -> Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let mut line = serde_json::to_string(rating)?;
    line.push('\n');

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;

    file.write_all(line.as_bytes()).await?;
    Ok(())
}
