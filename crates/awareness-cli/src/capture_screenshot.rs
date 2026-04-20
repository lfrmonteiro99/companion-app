//! Silent screen capture via xdg-desktop-portal Screenshot.
//!
//! GNOME/Mutter's PipeWire ScreenCast path hands out DmaBuf buffers that
//! `StreamFlags::MAP_BUFFERS` does not CPU-map, so `data.data()` returns
//! `None` on every tick even after negotiating `ParamBuffers(MemPtr|MemFd)`.
//! The portal Screenshot interface bypasses that entirely: on GNOME 46
//! xdg-desktop-portal-gnome dispatches the call directly to Mutter's
//! internal screenshot API (silent, no flash) and hands us a PNG file URI.
//!
//! Latency measured on a 2560x1440 display: ~1.2s per call. The 3s tick is
//! comfortably above that. For comparison `gnome-screenshot -f` is ~2.4s and
//! emits the shutter sound.
//!
//! The portal writes the PNG into `$XDG_PICTURES_DIR` and returns a
//! `file://` URI. We read the bytes and unlink — if the unlink fails we
//! still return the bytes (best effort).
#![cfg(feature = "portal")]

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::capture::{compute_hash, ScreenFrame};
use crate::config::Config;

/// Decode PNG bytes and wrap as a full-screen ScreenFrame with native_size
/// set so downstream a11y-bbox cropping can apply. `capture.rs::build_frame`
/// intentionally reports `native_size = None` for the sidecar (window-only)
/// path; this variant is for the full-screen Screenshot portal.
fn build_full_screen_frame(bytes: Vec<u8>) -> anyhow::Result<ScreenFrame> {
    use image::imageops::FilterType;

    let img = image::load_from_memory(&bytes)?;
    let native = (img.width(), img.height());
    let resized = img.resize(1280, 720, FilterType::Triangle);
    let hash = compute_hash(&resized);

    Ok(ScreenFrame {
        captured_at: chrono::Utc::now(),
        image: resized,
        perceptual_hash: hash,
        native_size: Some(native),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error("xdg-desktop-portal unavailable: {0}")]
    PortalUnavailable(String),
    #[error("user or compositor declined the screenshot")]
    Denied,
    #[error("unsupported screenshot URI scheme: {0}")]
    UnsupportedUri(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Take a single screenshot via the portal and return PNG bytes.
async fn capture_once() -> Result<Vec<u8>, ScreenshotError> {
    use ashpd::desktop::screenshot::Screenshot;

    let response = Screenshot::request()
        .interactive(false)
        .modal(false)
        .send()
        .await
        .map_err(classify_err)?
        .response()
        .map_err(classify_err)?;

    let uri = response.uri();
    if uri.scheme() != "file" {
        return Err(ScreenshotError::UnsupportedUri(uri.to_string()));
    }
    let path = uri
        .to_file_path()
        .map_err(|_| ScreenshotError::UnsupportedUri(uri.to_string()))?;

    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading screenshot file: {}", path.display()))
        .map_err(ScreenshotError::Other)?;

    if let Err(e) = tokio::fs::remove_file(&path).await {
        tracing::debug!(
            path = %path.display(),
            error = %e,
            "portal screenshot temp file not removed (best-effort)",
        );
    }

    Ok(bytes)
}

fn classify_err(e: ashpd::Error) -> ScreenshotError {
    use ashpd::Error as A;
    match e {
        A::Response(_) => ScreenshotError::Denied,
        A::PortalNotFound(iface) => {
            ScreenshotError::PortalUnavailable(format!("portal interface missing: {iface}"))
        }
        other => {
            let msg = other.to_string();
            let lower = msg.to_lowercase();
            if lower.contains("serviceunknown")
                || lower.contains("no such")
                || lower.contains("not provided by any")
            {
                ScreenshotError::PortalUnavailable(msg)
            } else {
                ScreenshotError::Other(anyhow::Error::new(other))
            }
        }
    }
}

/// Run the silent capture loop. Emits a `ScreenFrame` into `tx` every
/// `cfg.tick_screen_seconds`. Returns on any fatal error so the caller can
/// fall back to the sidecar path.
pub async fn run(tx: mpsc::Sender<ScreenFrame>, cfg: Arc<Config>) -> Result<(), ScreenshotError> {
    // Prove the portal is usable before the loop kicks in — if the first
    // call fails with PortalUnavailable/Denied, bubble up so the caller
    // picks the sidecar immediately instead of losing 3s per tick.
    let first_png = capture_once().await?;
    tracing::info!(
        "capture: screenshot portal active ({} KiB first frame)",
        first_png.len() / 1024
    );
    match build_full_screen_frame(first_png) {
        Ok(frame) => {
            let _ = tx.try_send(frame);
        }
        Err(e) => {
            tracing::warn!("failed to decode first screenshot: {e}");
        }
    }

    let tick = Duration::from_secs(cfg.tick_screen_seconds.max(1));
    loop {
        tokio::time::sleep(tick).await;
        match capture_once().await {
            Ok(png) => match build_full_screen_frame(png) {
                Ok(frame) => {
                    if tx.try_send(frame).is_err() {
                        tracing::debug!("capture: screen channel full — dropping");
                    }
                }
                Err(e) => tracing::warn!("failed to decode screenshot: {e}"),
            },
            Err(ScreenshotError::PortalUnavailable(msg)) => {
                // Hard fatal: portal went away mid-run. Bubble up.
                return Err(ScreenshotError::PortalUnavailable(msg));
            }
            Err(ScreenshotError::Denied) => {
                // Likely a lockscreen or privacy overlay. Skip this tick,
                // try again next one — the state is usually transient.
                tracing::debug!("capture: portal denied this tick (lockscreen?)");
            }
            Err(e) => {
                tracing::warn!("capture: screenshot tick failed: {e}");
            }
        }
    }
}
