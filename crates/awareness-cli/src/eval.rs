use anyhow::Result;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use awareness_core::types::{ContextEvent, FilterResponse};
use crate::gate::GateDecision;
use crate::tts::{self, TtsConfig};

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
    tts_config: TtsConfig,
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
            handle_prompt(&prompt, &ratings_path, &fallback_path, &mut line_rx, &tts_config).await;
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
    tts_config: &TtsConfig,
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

    tts::speak(quick_message, tts_config);

    let outcome = next_outcome(line_rx, 30).await;
    if let PromptOutcome::IoError(msg) = &outcome {
        tracing::error!("eval stdin read error: {msg}");
    }
    let rating_str = rating_from_outcome(&outcome);
    if rating_str == "invalid" {
        if let PromptOutcome::Input(raw) = &outcome {
            println!(
                "\x1b[33m(unrecognised input {:?}; logged as 'invalid' — use u/n/a or <enter>)\x1b[0m",
                raw.trim()
            );
        }
    }

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

/// Map a stdin outcome to the persisted rating string. Pure — used both
/// from the live loop and from tests.
///
/// Empty input (just Enter) → "skipped". Non-empty input that doesn't match
/// a rating letter → "invalid" so we can tell, after the fact, that the user
/// typed something unexpected rather than deliberately skipping.
fn rating_from_outcome(outcome: &PromptOutcome) -> &'static str {
    match outcome {
        PromptOutcome::Input(s) => {
            let trimmed = s.trim();
            match trimmed {
                "u" => "useful",
                "n" => "not_useful",
                "a" => "annoying",
                "" => "skipped",
                _ => "invalid",
            }
        }
        PromptOutcome::Timeout => "timeout",
        PromptOutcome::Eof => "skipped",
        PromptOutcome::IoError(_) => "error",
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
    // tokio::fs::File's Drop does not await the close — without an
    // explicit flush the bytes may still be buffered on the blocking
    // IO thread when the caller (or a test) reads the file back. Under
    // CI load this surfaces as a flaky "file is empty" failure.
    file.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "awareness-eval-{}-{}-{}",
            std::process::id(),
            tag,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        p
    }

    #[test]
    fn rating_map_useful_input() {
        assert_eq!(
            rating_from_outcome(&PromptOutcome::Input("u\n".into())),
            "useful"
        );
        assert_eq!(
            rating_from_outcome(&PromptOutcome::Input("n".into())),
            "not_useful"
        );
        assert_eq!(
            rating_from_outcome(&PromptOutcome::Input(" a ".into())),
            "annoying"
        );
    }

    #[test]
    fn rating_map_empty_input_is_skipped() {
        assert_eq!(rating_from_outcome(&PromptOutcome::Input("".into())), "skipped");
        assert_eq!(rating_from_outcome(&PromptOutcome::Input("  \n".into())), "skipped");
    }

    #[test]
    fn rating_map_unknown_input_is_invalid_not_skipped() {
        // Non-empty input that doesn't match a letter must be distinguishable
        // from a deliberate skip so analysis can flag accidental rubbish input.
        assert_eq!(rating_from_outcome(&PromptOutcome::Input("x".into())), "invalid");
        assert_eq!(rating_from_outcome(&PromptOutcome::Input("yes".into())), "invalid");
        assert_eq!(rating_from_outcome(&PromptOutcome::Input("1".into())), "invalid");
    }

    #[test]
    fn rating_map_timeout_vs_eof_vs_error_are_distinct() {
        assert_eq!(rating_from_outcome(&PromptOutcome::Timeout), "timeout");
        assert_eq!(rating_from_outcome(&PromptOutcome::Eof), "skipped");
        assert_eq!(
            rating_from_outcome(&PromptOutcome::IoError("pipe closed".into())),
            "error"
        );
        // "timeout" must not collide with "skipped" — the whole point of the
        // PromptOutcome refactor.
        assert_ne!(
            rating_from_outcome(&PromptOutcome::Timeout),
            rating_from_outcome(&PromptOutcome::Eof),
        );
    }

    #[tokio::test]
    async fn append_rating_writes_jsonl_line() {
        let path = unique_path("good");
        let rating = Rating {
            tick_id: 42,
            rating: "useful".into(),
            note: None,
        };
        append_rating(&path, &rating).await.expect("write must succeed");

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.ends_with('\n'), "JSONL entries must end with newline");
        let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(v["tick_id"], 42);
        assert_eq!(v["rating"], "useful");

        // Second append: file must grow, first line must survive.
        let rating2 = Rating {
            tick_id: 43,
            rating: "timeout".into(),
            note: Some("n/a".into()),
        };
        append_rating(&path, &rating2).await.unwrap();
        let raw2 = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = raw2.lines().collect();
        assert_eq!(lines.len(), 2, "two appends must produce two lines");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn append_rating_errors_on_unwritable_path() {
        // Pointing into /proc which refuses arbitrary writes — the call must
        // return Err so the fallback path in handle_prompt can take over.
        let path = PathBuf::from("/proc/awareness-cannot-write-here.jsonl");
        let rating = Rating {
            tick_id: 1,
            rating: "useful".into(),
            note: None,
        };
        let err = append_rating(&path, &rating).await;
        assert!(
            err.is_err(),
            "writing under /proc must fail so the fallback path kicks in"
        );
    }
}
