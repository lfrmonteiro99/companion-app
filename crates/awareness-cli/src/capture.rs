use anyhow::Result;
use chrono::{DateTime, Utc};
use image::DynamicImage;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct ScreenFrame {
    pub captured_at: DateTime<Utc>,
    pub image: DynamicImage,
    /// 0 when img_hash feature not enabled.
    pub perceptual_hash: u64,
}

/// Returns true if the hash distance is below threshold (frames are similar).
pub fn is_similar_frame(hash_a: u64, hash_b: u64, threshold: u32) -> bool {
    (hash_a ^ hash_b).count_ones() <= threshold
}

/// Spawns screen capture loop. Returns handle.
///
/// Strategy (in order of preference):
/// 1. ashpd ScreenCast portal (Wayland-native) — only when feature = "full"
/// 2. grim sidecar (subprocess, writes to stdout as PNG)
/// 3. gnome-screenshot sidecar (fallback if grim not found)
///
/// A frame is emitted every cfg.tick_screen_seconds.
/// If channel is full (capacity 4): newest frame is dropped (non-blocking send).
pub async fn spawn_screen_capture(
    tx: mpsc::Sender<ScreenFrame>,
    cfg: Arc<Config>,
) -> Result<JoinHandle<()>> {
    let handle = tokio::spawn(async move {
        capture_loop(tx, cfg).await;
    });
    Ok(handle)
}

async fn capture_loop(tx: mpsc::Sender<ScreenFrame>, cfg: Arc<Config>) {
    #[cfg(feature = "full")]
    {
        // Try the ashpd portal path first.
        match try_portal_capture(tx.clone(), cfg.clone()).await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!("Portal capture failed ({}), falling back to sidecar", e);
            }
        }
    }

    // Sidecar path: always compiled.
    sidecar_capture_loop(tx, cfg).await;
}

// ---------------------------------------------------------------------------
// Portal path (feature = "full")
// ---------------------------------------------------------------------------

#[cfg(feature = "full")]
async fn try_portal_capture(
    tx: mpsc::Sender<ScreenFrame>,
    cfg: Arc<Config>,
) -> Result<()> {
    // PipeWire frame consumption not yet implemented — fall back to sidecar.
    anyhow::bail!("Portal capture not yet implemented; using sidecar fallback");

    // Silence dead-code warnings for bindings used above.
    #[allow(unreachable_code)]
    {
        let _ = (tx, cfg);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sidecar path (always compiled)
// ---------------------------------------------------------------------------

async fn sidecar_capture_loop(tx: mpsc::Sender<ScreenFrame>, cfg: Arc<Config>) {
    loop {
        match capture_via_sidecar().await {
            Ok(bytes) => match build_frame(bytes) {
                Ok(frame) => {
                    if tx.try_send(frame).is_err() {
                        tracing::warn!("Screen frame channel full — dropping frame");
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to decode captured frame: {}", e);
                }
            },
            Err(e) => {
                tracing::warn!("Sidecar capture error: {}", e);
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(cfg.tick_screen_seconds)).await;
    }
}

/// Try `grim -`, then fall back to `gnome-screenshot`.
async fn capture_via_sidecar() -> Result<Vec<u8>> {
    use tokio::process::Command;

    // Try grim first (wlroots: Sway, Hyprland). Skip on GNOME/Mutter.
    let is_gnome = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_uppercase()
        .contains("GNOME");

    if !is_gnome {
        let grim_result = Command::new("grim").arg("-").output().await;
        match grim_result {
            Ok(output) if output.status.success() => return Ok(output.stdout),
            Ok(output) => tracing::warn!("grim: {}", String::from_utf8_lossy(&output.stderr)),
            Err(e) if is_command_not_found(&e) => {}
            Err(e) => tracing::warn!("grim spawn: {e}"),
        }
    }

    // gnome-screenshot (GNOME/XWayland)
    // Needs DISPLAY for XWayland; default to :0 if not set.
    const TMP_PATH: &str = "/tmp/awareness_shot.png";
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    // -w = active window only (not whole desktop). Much smaller image,
    // content-relevant OCR, title bar gives us the app name.
    let gs_result = Command::new("gnome-screenshot")
        .args(["-w", "-f", TMP_PATH])
        .env("DISPLAY", &display)
        .output()
        .await;

    match gs_result {
        Ok(output) if output.status.success() => {
            let bytes = tokio::fs::read(TMP_PATH).await?;
            Ok(bytes)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("gnome-screenshot failed: {}", stderr);
        }
        Err(e) => {
            anyhow::bail!("gnome-screenshot spawn error: {}", e);
        }
    }
}

fn is_command_not_found(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

fn build_frame(bytes: Vec<u8>) -> Result<ScreenFrame> {
    use image::imageops::FilterType;

    let img = image::load_from_memory(&bytes)?;
    let resized = img.resize(1280, 720, FilterType::Triangle);

    let hash = compute_hash(&resized);

    Ok(ScreenFrame {
        captured_at: Utc::now(),
        image: resized,
        perceptual_hash: hash,
    })
}

#[cfg(feature = "full")]
fn compute_hash(img: &DynamicImage) -> u64 {
    // Simple dHash: resize to 9x8, compare adjacent pixels per row → 64 bits.
    use image::imageops::{resize, FilterType};
    use image::GrayImage;

    let small = resize(img, 9, 8, FilterType::Triangle);
    let gray = image::DynamicImage::ImageRgba8(small).to_luma8();

    let mut hash = 0u64;
    let mut bit = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let left  = gray.get_pixel(x, y)[0];
            let right = gray.get_pixel(x + 1, y)[0];
            if left > right {
                hash |= 1 << bit;
            }
            bit += 1;
        }
    }
    hash
}

#[cfg(not(feature = "full"))]
fn compute_hash(_img: &DynamicImage) -> u64 {
    0u64
}
