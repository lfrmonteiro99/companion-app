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
    /// Native capture size before the 1280x720 resize, in screen pixels.
    /// `Some` only for full-screen captures (portal). `None` for window-only
    /// captures (sidecar gnome-screenshot -w) where the a11y bounding box
    /// wouldn't be meaningful. Used to map a11y bbox → `image` coordinates.
    pub native_size: Option<(u32, u32)>,
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
    // Preferred path: xdg-desktop-portal Screenshot (ashpd). On GNOME 46 this
    // is dispatched straight to Mutter's internal screenshot API: silent, no
    // flash, no per-tick dialog, ~1.2s per capture. Crucially it sidesteps
    // the PipeWire ScreenCast DmaBuf problem that left `data.data() == None`
    // on every buffer.
    //
    // `AWARENESS_USE_PIPEWIRE=1` force-enables the (currently broken on
    // Mutter) PipeWire path for debugging. `AWARENESS_USE_PORTAL=0` skips
    // both portal paths and goes straight to the sidecar (noisy but reliable
    // wherever gnome-screenshot / grim works).
    #[cfg(feature = "portal")]
    {
        let portal_allowed = std::env::var("AWARENESS_USE_PORTAL")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(true);

        if portal_allowed {
            // 1. ScreenCast portal + PipeWire stream. TRUE silent: Mutter
            //    treats this as a continuous video capture, no per-frame
            //    shutter sound, no flash. `param_changed` now publishes
            //    ParamBuffers + ParamMeta(Header) + ParamMeta(VideoTransform)
            //    via stream.update_params — the handshake Mutter requires to
            //    actually deliver CPU-mappable buffers.
            let use_pipewire = std::env::var("AWARENESS_USE_PIPEWIRE")
                .map(|v| v != "0" && !v.is_empty())
                .unwrap_or(true);
            if use_pipewire {
                use crate::capture_portal::{self, PortalError};
                tracing::info!("capture: trying xdg-desktop-portal ScreenCast (silent)");
                match capture_portal::run(tx.clone(), cfg.clone()).await {
                    Ok(()) => return,
                    Err(PortalError::UserCancelled) => {
                        tracing::warn!(
                            "ScreenCast permission declined — trying Screenshot portal next."
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "ScreenCast failed: {e}. Trying Screenshot portal next."
                        );
                    }
                }
            }

            // 2. Screenshot portal via ashpd. NOT truly silent on GNOME —
            //    Mutter still emits the shutter sound because this hits the
            //    per-shot screenshot API. Kept as a fallback for hosts
            //    where ScreenCast is broken, with the audible trade-off
            //    documented. Set AWARENESS_USE_SCREENSHOT=0 to skip.
            let use_screenshot = std::env::var("AWARENESS_USE_SCREENSHOT")
                .map(|v| v != "0" && !v.is_empty())
                .unwrap_or(true);
            if use_screenshot {
                use crate::capture_screenshot::{self, ScreenshotError};
                tracing::warn!(
                    "capture: Screenshot portal active (per-tick shutter sound on GNOME)"
                );
                match capture_screenshot::run(tx.clone(), cfg.clone()).await {
                    Ok(()) => return,
                    Err(ScreenshotError::PortalUnavailable(msg)) => {
                        tracing::info!("Screenshot portal unavailable ({msg}) — using sidecar.");
                    }
                    Err(ScreenshotError::Denied) => {
                        tracing::warn!(
                            "Screenshot portal declined on the first call — using sidecar."
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Screenshot portal failed: {e}. Falling back to sidecar.");
                    }
                }
            }
        } else {
            tracing::info!("capture: AWARENESS_USE_PORTAL=0 — skipping portal");
        }
    }

    tracing::info!("capture: using sidecar (gnome-screenshot / grim)");
    sidecar_capture_loop(tx, cfg).await;
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

/// Decode a PNG/JPEG byte buffer into a `ScreenFrame` with a real perceptual
/// hash. Public so the portal Screenshot path can reuse it.
pub fn build_frame(bytes: Vec<u8>) -> Result<ScreenFrame> {
    use image::imageops::FilterType;

    let img = image::load_from_memory(&bytes)?;
    let resized = img.resize(1280, 720, FilterType::Triangle);

    let hash = compute_hash(&resized);

    Ok(ScreenFrame {
        captured_at: Utc::now(),
        image: resized,
        perceptual_hash: hash,
        // Sidecar (gnome-screenshot -w) captures the window, not the whole
        // screen — a11y bbox (screen coords) doesn't apply here.
        native_size: None,
    })
}

/// dHash: resize to 9x8 greyscale, compare adjacent pixels per row → 64 bits.
/// Must be feature-agnostic: the dedup layer drops every frame after the
/// first when every hash collides to 0, which is what happened historically
/// under `--features "portal ocr"` (no `full`).
pub fn compute_hash(img: &DynamicImage) -> u64 {
    use image::imageops::{resize, FilterType};

    let small = resize(&img.to_luma8(), 9, 8, FilterType::Triangle);

    let mut hash = 0u64;
    let mut bit = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let left = small.get_pixel(x, y)[0];
            let right = small.get_pixel(x + 1, y)[0];
            if left > right {
                hash |= 1 << bit;
            }
            bit += 1;
        }
    }
    hash
}
