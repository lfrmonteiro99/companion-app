//! Wayland ScreenCast portal + PipeWire frame consumption.
//!
//! Negotiates a one-time permission prompt via `xdg-desktop-portal` and then
//! consumes frames from PipeWire silently (no flash, no shutter sound). A
//! restore token is persisted to `<output_dir>/screencast_restore.token` so
//! subsequent runs reuse the granted permission without prompting again.
//!
//! This module is only compiled with `--features full` (needs `ashpd` and
//! `pipewire` crates, which pull in native deps).

#![cfg(feature = "portal")]

use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use image::{DynamicImage, RgbaImage};
use tokio::sync::mpsc;

use crate::capture::ScreenFrame;
use crate::config::Config;

#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    #[error("user declined screen capture permission")]
    UserCancelled,
    #[error("no xdg-desktop-portal service available: {0}")]
    NoPortal(String),
    #[error("portal negotiation failed: {0}")]
    Negotiation(String),
    #[error("PipeWire error: {0}")]
    PipeWire(String),
}

/// Run the portal capture loop. Returns only on fatal error — the caller
/// should then fall back to sidecar capture.
pub async fn run(tx: mpsc::Sender<ScreenFrame>, cfg: Arc<Config>) -> Result<(), PortalError> {
    let token_path = cfg.output_dir.join("screencast_restore.token");
    let session = negotiate_session(&token_path).await?;

    tracing::info!(
        "portal: session ready, pw_fd={} node_id={}",
        session.pw_fd.as_raw_fd(),
        session.node_id,
    );

    let tick_secs = cfg.tick_screen_seconds;
    let node_id = session.node_id;
    let pw_fd = session.pw_fd;

    // PipeWire mainloop is !Send — must run on a dedicated OS thread.
    let (err_tx, mut err_rx) = mpsc::channel::<String>(1);
    std::thread::Builder::new()
        .name("pw-screencast".into())
        .spawn(move || {
            if let Err(e) = run_pipewire_loop(pw_fd, node_id, tx, tick_secs) {
                let _ = err_tx.blocking_send(e.to_string());
            }
        })
        .map_err(|e| PortalError::PipeWire(format!("thread spawn: {e}")))?;

    match err_rx.recv().await {
        Some(msg) => Err(PortalError::PipeWire(msg)),
        None => Ok(()), // thread exited cleanly (shouldn't happen under normal use)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// ashpd: one-time permission + restore token persistence
// ───────────────────────────────────────────────────────────────────────────

struct PortalSession {
    pw_fd: OwnedFd,
    node_id: u32,
}

async fn negotiate_session(token_path: &Path) -> Result<PortalSession, PortalError> {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::PersistMode;
    use ashpd::WindowIdentifier;

    let proxy = Screencast::new()
        .await
        .map_err(|e| classify_ashpd_error(&e))?;

    let session = proxy
        .create_session()
        .await
        .map_err(|e| PortalError::Negotiation(format!("create_session: {e}")))?;

    let restore_token = load_token(token_path);

    proxy
        .select_sources(
            &session,
            CursorMode::Hidden,
            SourceType::Monitor.into(),
            false, // multiple sources
            restore_token.as_deref(),
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(|e| classify_ashpd_error(&e))?;

    let response = proxy
        .start(&session, &WindowIdentifier::default())
        .await
        .map_err(|e| classify_ashpd_error(&e))?
        .response()
        .map_err(|e| classify_ashpd_error(&e))?;

    let streams = response.streams();
    let stream = streams
        .first()
        .ok_or_else(|| PortalError::Negotiation("portal returned no streams".into()))?;
    let node_id = stream.pipe_wire_node_id();

    if let Some(token) = response.restore_token() {
        save_token(token_path, token);
    }

    let pw_fd = proxy
        .open_pipe_wire_remote(&session)
        .await
        .map_err(|e| PortalError::Negotiation(format!("open_pipe_wire_remote: {e}")))?;

    Ok(PortalSession { pw_fd, node_id })
}

fn classify_ashpd_error(e: &ashpd::Error) -> PortalError {
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    if lower.contains("cancelled") || lower.contains("denied") {
        PortalError::UserCancelled
    } else if lower.contains("serviceunknown")
        || lower.contains("not provided by any")
        || lower.contains("no such")
    {
        PortalError::NoPortal(msg)
    } else {
        PortalError::Negotiation(msg)
    }
}

fn load_token(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn save_token(path: &Path, token: &str) {
    // Best-effort; log but don't fail capture. 0600 perms so other users
    // can't hijack the restore token.
    if let Err(e) = std::fs::write(path, token) {
        tracing::warn!("portal: failed to save restore token to {:?}: {e}", path);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

// ───────────────────────────────────────────────────────────────────────────
// PipeWire: dedicated mainloop thread, Input stream, dequeue frames
// ───────────────────────────────────────────────────────────────────────────

fn run_pipewire_loop(
    pw_fd: OwnedFd,
    node_id: u32,
    tx: mpsc::Sender<ScreenFrame>,
    min_tick_secs: u64,
) -> Result<()> {
    use pipewire as pw;

    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None)
        .context("pipewire MainLoop::new")?;
    let context = pw::context::Context::new(&mainloop).context("pipewire Context::new")?;
    let core = context
        .connect_fd(pw_fd, None)
        .context("pipewire connect_fd")?;

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };

    let stream = pw::stream::Stream::new(&core, "awareness-screencast", props)
        .context("pipewire Stream::new")?;

    // Frame format state — learned from param_changed events. Until we know
    // the real width/height/format, we drop buffers rather than guess.
    let fmt_state = std::sync::Arc::new(std::sync::Mutex::new(FrameFormat::default()));

    let min_interval = std::time::Duration::from_secs(min_tick_secs.max(1));
    let last_emit = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now() - min_interval));

    let fmt_for_param = fmt_state.clone();
    let fmt_for_process = fmt_state.clone();
    let last_emit_for_process = last_emit.clone();
    let tx_for_process = tx.clone();

    let _listener = stream
        .add_local_listener::<()>()
        .param_changed(move |_stream, _user, id, pod| {
            // id is the param id; ignore non-Format params.
            if id != pipewire::spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Some(pod) = pod {
                if let Some(info) = parse_video_format(pod) {
                    tracing::info!(
                        "portal: video format negotiated: {}x{} fourcc={:?}",
                        info.width, info.height, info.fourcc
                    );
                    if let Ok(mut g) = fmt_for_param.lock() {
                        *g = info;
                    }
                }
            }
        })
        .process(move |stream, _user| {
            // Throttle to min_interval to match cfg.tick_screen_seconds.
            let now = std::time::Instant::now();
            {
                let mut last = match last_emit_for_process.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                if now.duration_since(*last) < min_interval {
                    // Drain the buffer without producing a frame.
                    if let Some(buf) = stream.dequeue_buffer() {
                        drop(buf);
                    }
                    return;
                }
                *last = now;
            }

            let fmt = match fmt_for_process.lock() {
                Ok(g) => g.clone(),
                Err(_) => return,
            };
            if !fmt.is_valid() {
                // Format not yet negotiated — skip.
                if let Some(buf) = stream.dequeue_buffer() {
                    drop(buf);
                }
                return;
            }

            let Some(mut buffer) = stream.dequeue_buffer() else { return; };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let Some(bytes) = data.data() else { return; };
            if bytes.len() < (fmt.width as usize * fmt.height as usize * 4) {
                return;
            }

            match bytes_to_frame(bytes, &fmt) {
                Some(frame) => {
                    if tx_for_process.try_send(frame).is_err() {
                        tracing::debug!("portal: frame channel full — dropping");
                    }
                }
                None => {
                    tracing::debug!("portal: unsupported pixel format; dropping frame");
                }
            }
        })
        .register()
        .context("pipewire stream listener register")?;

    // Connect with an empty param list — compositor will pick a format and
    // we learn it through param_changed. Input direction = we consume.
    stream
        .connect(
            pipewire::spa::utils::Direction::Input,
            Some(node_id),
            pipewire::stream::StreamFlags::AUTOCONNECT | pipewire::stream::StreamFlags::MAP_BUFFERS,
            &mut [],
        )
        .context("pipewire stream connect")?;

    tracing::info!("portal: pipewire mainloop running");
    mainloop.run();
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// Frame format + pixel conversion
// ───────────────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct FrameFormat {
    width: u32,
    height: u32,
    /// Pixel fourcc as interpreted for our conversion. Only a few common
    /// layouts are supported; others cause the frame to be dropped.
    fourcc: PixelFormat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PixelFormat {
    #[default]
    Unknown,
    /// 4 bytes per pixel, order B, G, R, X (alpha unused) — GNOME default.
    Bgrx,
    /// 4 bytes per pixel, order R, G, B, X.
    Rgbx,
    /// 4 bytes per pixel, order B, G, R, A.
    Bgra,
    /// 4 bytes per pixel, order R, G, B, A.
    Rgba,
}

impl FrameFormat {
    fn is_valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.fourcc != PixelFormat::Unknown
    }
}

/// Best-effort SPA video format parser. SPA pod layouts are verbose; we only
/// need width/height/format out of the negotiated Format object, and we
/// accept a conservative set of pixel formats.
///
/// If parsing fails, frames are dropped and the portal path becomes a no-op
/// (the outer loop's error channel will eventually trigger a sidecar fallback
/// at next session restart).
fn parse_video_format(pod: &pipewire::spa::pod::Pod) -> Option<FrameFormat> {
    use pipewire::spa::pod::deserialize::PodDeserializer;
    use pipewire::spa::pod::{Object, Value};

    let (_, value) = PodDeserializer::deserialize_any_from(pod.as_bytes()).ok()?;
    let Value::Object(Object { properties, .. }) = value else {
        return None;
    };

    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut fourcc = PixelFormat::Unknown;

    for prop in properties {
        // Property keys are u32 IDs from libspa's format enum. We match by
        // numeric id to avoid pulling in the full libspa-sys constant set.
        // 1 = mediaType, 2 = mediaSubtype, 3 = format, 4 = size.
        match prop.key {
            3 => {
                if let Value::Id(id) = prop.value {
                    fourcc = map_spa_video_format(id.0);
                }
            }
            4 => {
                if let Value::Rectangle(r) = prop.value {
                    width = r.width;
                    height = r.height;
                }
            }
            _ => {}
        }
    }

    Some(FrameFormat { width, height, fourcc })
}

/// Mapping from SPA VideoFormat enum IDs to our simplified pixel layout.
/// Values come from `spa/param/video/raw.h` — stable across SPA versions.
fn map_spa_video_format(id: u32) -> PixelFormat {
    match id {
        7 | 15 => PixelFormat::Rgbx, // RGBx / xRGB variants
        8      => PixelFormat::Bgrx, // BGRx
        11     => PixelFormat::Rgba,
        12     => PixelFormat::Bgra,
        _      => PixelFormat::Unknown,
    }
}

fn bytes_to_frame(bytes: &[u8], fmt: &FrameFormat) -> Option<ScreenFrame> {
    let needed = fmt.width as usize * fmt.height as usize * 4;
    let src = bytes.get(..needed)?;
    let mut rgba = Vec::with_capacity(needed);
    match fmt.fourcc {
        PixelFormat::Rgba | PixelFormat::Rgbx => {
            rgba.extend_from_slice(src);
        }
        PixelFormat::Bgra | PixelFormat::Bgrx => {
            for chunk in src.chunks_exact(4) {
                rgba.push(chunk[2]);
                rgba.push(chunk[1]);
                rgba.push(chunk[0]);
                rgba.push(chunk[3]);
            }
        }
        PixelFormat::Unknown => return None,
    }

    let img = RgbaImage::from_raw(fmt.width, fmt.height, rgba)?;
    let dyn_img = DynamicImage::ImageRgba8(img);
    // Resize to match the sidecar path — downstream OCR/vision expects 1280x720.
    let resized = dyn_img.resize(1280, 720, image::imageops::FilterType::Triangle);

    Some(ScreenFrame {
        captured_at: Utc::now(),
        image: resized,
        perceptual_hash: 0, // dedup handles this upstream via hash compare
    })
}
