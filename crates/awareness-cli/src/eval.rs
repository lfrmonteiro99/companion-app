use anyhow::Result;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::AsyncBufReadExt as _;
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
    pub rating: String, // "useful" | "not_useful" | "annoying" | "skipped"
    pub note: Option<String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Spawns the eval display loop.
/// Receives AlertPrompts, displays them, collects ratings, writes to ratings_path.
pub async fn spawn_eval_loop(
    mut alert_rx: mpsc::Receiver<AlertPrompt>,
    ratings_path: PathBuf,
) -> Result<JoinHandle<()>> {
    let handle = tokio::spawn(async move {
        while let Some(prompt) = alert_rx.recv().await {
            handle_prompt(&prompt, &ratings_path).await;
        }
    });

    Ok(handle)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

async fn handle_prompt(prompt: &AlertPrompt, ratings_path: &PathBuf) {
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

    let raw_input = read_line_with_timeout(30).await;
    let rating_str = match raw_input.as_deref().map(str::trim) {
        Some("u") => "useful",
        Some("n") => "not_useful",
        Some("a") => "annoying",
        _ => "skipped",
    };

    let rating = Rating {
        tick_id: prompt.tick_id,
        rating: rating_str.to_string(),
        note: None,
    };

    if let Err(e) = append_rating(ratings_path, &rating).await {
        tracing::warn!("Failed to write rating: {:?}", e);
    }
}

/// Read one line from stdin, returning None on timeout or error.
async fn read_line_with_timeout(timeout_secs: u64) -> Option<String> {
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = String::new();

    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        reader.read_line(&mut line),
    )
    .await;

    match result {
        Ok(Ok(_)) => Some(line),
        _ => None,
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
