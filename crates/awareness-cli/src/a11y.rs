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
    /// [x, y, width, height] in screen pixels when the Component interface
    /// exposes the focused window's extents. Absent when the app is behind
    /// mutter-x11-frames or doesn't implement Component.
    #[serde(default)]
    bbox: Option<[i32; 4]>,
    /// Set to `true` by the Python side when it successfully located the
    /// active window but its subtree is too thin to feed the model (VS Code
    /// Monaco editor, most Electron canvas renders). Callers should fall
    /// back to OCR but still use `bbox` to crop to the focused window.
    #[serde(default)]
    thin: bool,
}

/// Hint returned when the a11y tree doesn't have enough text to replace OCR,
/// but the focused window is known (with a bounding box). OCR can then be
/// run on the cropped region instead of the whole desktop.
#[derive(Debug, Clone)]
pub struct A11yHint {
    pub inferred_app_name: Option<String>,
    pub title: String,
    pub active_bbox: Option<(i32, i32, u32, u32)>,
}

pub enum A11yResult {
    /// Rich a11y tree — use as-is, skip OCR.
    Rich(OcrOutput),
    /// a11y found the window but its subtree is thin; OCR should run on the
    /// cropped region.
    Thin(A11yHint),
    /// No active window or the sidecar failed outright.
    None,
}

/// Call the Python AT-SPI sidecar and classify the result. Returns `Rich`
/// when we got enough structured text to skip OCR, `Thin` when the active
/// window was located but its subtree was too small (Electron canvas, VS
/// Code Monaco editor) — in that case the caller should still use the bbox
/// to crop before running OCR — and `None` when the sidecar failed outright.
pub async fn try_snapshot(
    script_path: &Path,
    captured_at: DateTime<Utc>,
    min_chars: usize,
    min_nodes: usize,
) -> Result<A11yResult> {
    let run = Command::new("python3")
        .arg(script_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let output = match timeout(Duration::from_secs(4), run).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::debug!("a11y sidecar spawn failed: {e}");
            return Ok(A11yResult::None);
        }
        Err(_) => {
            tracing::debug!("a11y sidecar timed out");
            return Ok(A11yResult::None);
        }
    };

    if !output.status.success() {
        return Ok(A11yResult::None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let snap: A11ySnapshot = match serde_json::from_str(stdout.trim()) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("a11y JSON parse failed: {e} | raw={}", stdout.trim());
            return Ok(A11yResult::None);
        }
    };

    if let Some(err) = snap.error {
        tracing::debug!("a11y sidecar: {err}");
        return Ok(A11yResult::None);
    }

    let active_bbox = snap.bbox.and_then(|b| {
        let w = i32::max(0, b[2]) as u32;
        let h = i32::max(0, b[3]) as u32;
        if w > 0 && h > 0 {
            Some((b[0], b[1], w, h))
        } else {
            None
        }
    });
    let inferred_app_name = map_a11y_app(&snap.app).or_else(|| infer_app_name(&snap.title));

    // Rich path only when both the text is long enough AND the tree is
    // wide enough. Electron/VS Code with --force-renderer-accessibility
    // often has many nodes but empty text (toolbars, icons) and should
    // still fall through to OCR+crop.
    let is_thin = snap.thin
        || snap.text.chars().count() < min_chars
        || snap.nodes < min_nodes;

    if is_thin {
        tracing::info!(
            "a11y thin (using bbox only): app={:?} title={:?} nodes={} chars={} bbox={:?}",
            snap.app,
            snap.title,
            snap.nodes,
            snap.text.chars().count(),
            active_bbox,
        );
        return Ok(A11yResult::Thin(A11yHint {
            inferred_app_name,
            title: snap.title,
            active_bbox,
        }));
    }

    tracing::info!(
        "a11y hit: raw_app={:?} title={:?} nodes={} chars={} canonical={:?}",
        snap.app,
        snap.title,
        snap.nodes,
        snap.text.chars().count(),
        inferred_app_name,
    );

    Ok(A11yResult::Rich(OcrOutput {
        captured_at,
        full_text: snap.text,
        title_bar_text: snap.title,
        inferred_app_name,
        active_bbox,
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
