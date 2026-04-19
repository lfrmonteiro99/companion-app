use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::ocr::{infer_app_name, OcrOutput};

#[derive(Debug, Deserialize)]
struct A11ySnapshot {
    #[serde(default)]
    app: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    nodes: usize,
    #[serde(default)]
    error: Option<String>,
}

/// Call the Python AT-SPI sidecar and return an `OcrOutput` if the snapshot
/// is rich enough to use in place of OCR. Returns `Ok(None)` when the snapshot
/// is too thin (no focused window, Electron/Chromium apps without a11y flag,
/// etc.) — callers should fall back to OCR in that case.
pub async fn try_snapshot(
    script_path: &Path,
    captured_at: DateTime<Utc>,
    min_chars: usize,
    min_nodes: usize,
) -> Result<Option<OcrOutput>> {
    let run = Command::new("python3")
        .arg(script_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let output = match timeout(Duration::from_secs(4), run).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::debug!("a11y sidecar spawn failed: {e}");
            return Ok(None);
        }
        Err(_) => {
            tracing::debug!("a11y sidecar timed out");
            return Ok(None);
        }
    };

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let snap: A11ySnapshot = match serde_json::from_str(stdout.trim()) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("a11y JSON parse failed: {e} | raw={}", stdout.trim());
            return Ok(None);
        }
    };

    if let Some(err) = snap.error {
        tracing::debug!("a11y sidecar: {err}");
        return Ok(None);
    }

    // Too thin → caller should fall back to OCR.
    if snap.text.chars().count() < min_chars || snap.nodes < min_nodes {
        tracing::info!(
            "a11y thin: app={:?} title={:?} nodes={} chars={}",
            snap.app,
            snap.title,
            snap.nodes,
            snap.text.chars().count()
        );
        return Ok(None);
    }

    // Map a11y raw name → canonical name (same vocabulary as OCR fallback).
    let inferred_app_name = map_a11y_app(&snap.app).or_else(|| infer_app_name(&snap.title));

    tracing::info!(
        "a11y hit: raw_app={:?} title={:?} nodes={} chars={} canonical={:?}",
        snap.app,
        snap.title,
        snap.nodes,
        snap.text.chars().count(),
        inferred_app_name,
    );

    Ok(Some(OcrOutput {
        captured_at,
        full_text: snap.text,
        title_bar_text: snap.title,
        inferred_app_name,
    }))
}

fn map_a11y_app(raw: &str) -> Option<String> {
    let lower = raw.to_lowercase();
    // Only hard-code the ones where the a11y registration name differs
    // from what infer_app_name would derive from the title.
    const MAP: &[(&str, &str)] = &[
        ("teams-for-linux", "teams"),
        ("google chrome", "chrome"),
        ("chromium", "chrome"),
        ("gnome-terminal-server", "terminal"),
        ("ptyxis", "terminal"),
        ("gnome-text-editor", "text_editor"),
        ("nemo", "files"),
        ("nautilus", "files"),
        ("code", "vscode"),
        ("code - oss", "vscode"),
        ("firefox", "firefox"),
    ];
    for (needle, canonical) in MAP {
        if lower.contains(needle) {
            return Some((*canonical).to_string());
        }
    }
    None
}
