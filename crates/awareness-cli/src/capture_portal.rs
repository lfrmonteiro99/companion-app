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

/// How long to wait for the first frame before declaring the portal dead
/// and falling back to sidecar. Longer than a single tick to absorb session
/// negotiation jitter, short enough that the user doesn't sit without alerts.
const PORTAL_FIRST_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

    // Shared counter incremented each time the pipewire thread successfully
    // emits a frame to the outer channel. Used by the watchdog below.
    let frames_emitted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let frames_for_thread = frames_emitted.clone();

    // PipeWire mainloop is !Send — must run on a dedicated OS thread.
    let (err_tx, mut err_rx) = mpsc::channel::<String>(1);
    std::thread::Builder::new()
        .name("pw-screencast".into())
        .spawn(move || {
            if let Err(e) = run_pipewire_loop(pw_fd, node_id, tx, tick_secs, frames_for_thread) {
                let _ = err_tx.blocking_send(e.to_string());
            }
        })
        .map_err(|e| PortalError::PipeWire(format!("thread spawn: {e}")))?;

    // Watchdog: if no frame has been emitted after PORTAL_FIRST_FRAME_TIMEOUT,
    // we return an error so the caller falls back to the sidecar. This
    // catches the "pipewire stream stuck in configuring state, data.data()
    // always None" class of failure without forcing the user to interrupt.
    tokio::select! {
        msg = err_rx.recv() => match msg {
            Some(e) => Err(PortalError::PipeWire(e)),
            None => Ok(()),
        },
        _ = tokio::time::sleep(PORTAL_FIRST_FRAME_TIMEOUT) => {
            let n = frames_emitted.load(std::sync::atomic::Ordering::Relaxed);
            if n == 0 {
                Err(PortalError::PipeWire(format!(
                    "no frames delivered in {:?} — stream stuck, falling back",
                    PORTAL_FIRST_FRAME_TIMEOUT,
                )))
            } else {
                // First frame arrived in time; keep consuming from pipewire
                // until it errors out.
                tracing::info!("portal: {n} frame(s) delivered within watchdog window — staying on portal");
                match err_rx.recv().await {
                    Some(e) => Err(PortalError::PipeWire(e)),
                    None => Ok(()),
                }
            }
        }
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
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
    frames_emitted: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Result<()> {
    use pipewire as pw;

    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None).context("pipewire MainLoop::new")?;
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
    let last_emit = std::sync::Arc::new(std::sync::Mutex::new(
        std::time::Instant::now() - min_interval,
    ));

    let fmt_for_param = fmt_state.clone();
    let fmt_for_process = fmt_state.clone();
    let last_emit_for_process = last_emit.clone();
    let tx_for_process = tx.clone();
    let frames_for_process = frames_emitted.clone();

    let _listener = stream
        .add_local_listener::<()>()
        .param_changed(move |stream, _user, id, pod| {
            // id is the param id; ignore non-Format params.
            if id != pipewire::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(pod) = pod else { return };
            let Some(info) = parse_video_format(pod) else {
                return;
            };
            tracing::info!(
                "portal: video format negotiated: {}x{} fourcc={:?}",
                info.width,
                info.height,
                info.fourcc
            );
            if let Ok(mut g) = fmt_for_param.lock() {
                *g = info.clone();
            }
            // Publish the client's buffer + meta requirements on EVERY format
            // negotiation event — including the compositor's initial 0x0
            // "proposal". Mutter needs to see the client's ParamBuffers
            // (MemPtr|MemFd) and ParamMeta(Header, VideoTransform) before it
            // fixates the pool. Wait-until-valid here and it deadlocks — the
            // second param_changed never arrives, the watchdog fires at 10 s.
            if let Err(e) = publish_stream_params(stream) {
                tracing::warn!("portal: update_params failed: {e}");
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

            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let need = fmt.width as usize * fmt.height as usize * 4;

            // Prefer the pre-mapped path. MAP_BUFFERS handles MemPtr/MemFd
            // for us; `data.data()` is then Some(&[u8]). For DmaBuf (which
            // Mutter still delivers on many GPU configs despite our
            // ParamBuffers(MemPtr|MemFd) hint) we fall through to a manual
            // mmap below.
            if let Some(bytes) = data.data() {
                if bytes.len() >= need {
                    if let Some(frame) = bytes_to_frame(bytes, &fmt) {
                        match tx_for_process.try_send(frame) {
                            Ok(()) => {
                                frames_for_process
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            Err(_) => tracing::debug!("portal: frame channel full — dropping"),
                        }
                    }
                }
                return;
            }

            // Fallback for DmaBuf / unmapped MemFd: manual mmap using the
            // fd + maxsize + mapoffset from the raw spa_data.
            let raw = data.as_raw();
            if raw.fd < 0 {
                tracing::debug!(
                    "portal: data.data()=None and fd={}; type_={:?} — skipping",
                    raw.fd,
                    data.type_()
                );
                return;
            }
            let map_len = raw.maxsize as usize;
            if map_len < need {
                tracing::debug!(
                    "portal: raw.maxsize {} < need {}; type_={:?}",
                    map_len,
                    need,
                    data.type_()
                );
                return;
            }
            unsafe {
                let ptr = libc::mmap(
                    std::ptr::null_mut(),
                    map_len,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    raw.fd as i32,
                    raw.mapoffset as i64,
                );
                if ptr == libc::MAP_FAILED {
                    let err = std::io::Error::last_os_error();
                    tracing::warn!(
                        "portal: mmap fd={} offset={} len={} failed: {}",
                        raw.fd,
                        raw.mapoffset,
                        map_len,
                        err
                    );
                    return;
                }
                let bytes = std::slice::from_raw_parts(ptr as *const u8, map_len);
                let frame_opt = bytes_to_frame(&bytes[..need], &fmt);
                libc::munmap(ptr, map_len);
                if let Some(frame) = frame_opt {
                    match tx_for_process.try_send(frame) {
                        Ok(()) => {
                            frames_for_process
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        Err(_) => tracing::debug!("portal: frame channel full — dropping"),
                    }
                }
            }
        })
        .register()
        .context("pipewire stream listener register")?;

    // Build the EnumFormat pod declaring which video formats we accept.
    // Connects publishes only this at stream.connect(); ParamBuffers and
    // ParamMeta are sent afterwards via stream.update_params() from inside
    // the `param_changed` callback (see `publish_stream_params`).
    let format_bytes = build_enum_format_bytes()?;
    let format_pod = pipewire::spa::pod::Pod::from_bytes(&format_bytes)
        .ok_or_else(|| anyhow::anyhow!("Pod::from_bytes failed for EnumFormat"))?;
    let mut params = [format_pod];

    stream
        .connect(
            pipewire::spa::utils::Direction::Input,
            Some(node_id),
            pipewire::stream::StreamFlags::AUTOCONNECT | pipewire::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .context("pipewire stream connect")?;

    tracing::info!("portal: pipewire mainloop running");
    mainloop.run();
    Ok(())
}

/// Serialize the EnumFormat pod sent at stream.connect() time. Declares
/// MediaType=Video, MediaSubtype=Raw, the 4 bpp RGB formats our pixel
/// converter understands, and flexible size/framerate ranges. No
/// `VideoModifier` property — omitting it tells the producer we cannot
/// handle modifier-aware DmaBuf, nudging it toward an SHM pool.
fn build_enum_format_bytes() -> Result<Vec<u8>> {
    use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use pipewire::spa::param::video::VideoFormat;
    use pipewire::spa::param::ParamType;
    use pipewire::spa::pod::serialize::PodSerializer;
    use pipewire::spa::pod::Value;
    use pipewire::spa::utils::{Fraction, Rectangle, SpaTypes};

    let obj = pipewire::spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pipewire::spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pipewire::spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pipewire::spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::RGBx,
            VideoFormat::BGRA,
            VideoFormat::RGBA
        ),
        pipewire::spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle { width: 1920, height: 1080 },
            Rectangle { width: 1, height: 1 },
            Rectangle { width: 8192, height: 8192 }
        ),
        pipewire::spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 30, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: 240, denom: 1 }
        ),
    );
    let bytes: Vec<u8> = PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .context("serialize EnumFormat pod")?
    .0
    .into_inner();
    Ok(bytes)
}

/// Publish ParamBuffers + ParamMeta(Header) + ParamMeta(VideoTransform) via
/// `stream.update_params()`. This is the handshake Mutter / xdg-desktop-
/// portal-gnome require after format fixation to transition the stream into
/// the data-carrying phase; skip it and `data.data()` stays None on every
/// `.process()` tick despite `chunk.size` being populated.
fn publish_stream_params(stream: &pipewire::stream::StreamRef) -> Result<()> {
    use pipewire::spa::pod::serialize::PodSerializer;
    use pipewire::spa::pod::{Object, Pod, Property, PropertyFlags, Value};

    // SPA property IDs (no enum wrapper in libspa-rs 0.8 for these).
    const SPA_PARAM_BUFFERS_DATA_TYPE: u32 = 6;
    const SPA_PARAM_META_TYPE: u32 = 1;
    const SPA_PARAM_META_SIZE: u32 = 2;
    // SPA meta type ids — from libspa-sys bindings.
    const SPA_META_HEADER: i32 = 1;
    const SPA_META_VIDEO_TRANSFORM: i32 = 8;
    const SIZEOF_SPA_META_HEADER: i32 = 32;
    const SIZEOF_SPA_META_VIDEO_TRANSFORM: i32 = 4;
    // (1<<MemPtr)|(1<<MemFd) = 2 | 4 = 6. Forbids DmaBuf — Mutter then
    // allocates an SHM pool that MAP_BUFFERS can CPU-map.
    const CPU_DATA_TYPES_MASK: i32 = (1 << 1) | (1 << 2);

    let buffers_obj = Object {
        type_: pipewire::spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: pipewire::spa::param::ParamType::Buffers.as_raw(),
        properties: vec![Property {
            key: SPA_PARAM_BUFFERS_DATA_TYPE,
            flags: PropertyFlags::empty(),
            value: Value::Int(CPU_DATA_TYPES_MASK),
        }],
    };
    let meta_header_obj = Object {
        type_: pipewire::spa::utils::SpaTypes::ObjectParamMeta.as_raw(),
        id: pipewire::spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            Property {
                key: SPA_PARAM_META_TYPE,
                flags: PropertyFlags::empty(),
                value: Value::Id(pipewire::spa::utils::Id(SPA_META_HEADER as u32)),
            },
            Property {
                key: SPA_PARAM_META_SIZE,
                flags: PropertyFlags::empty(),
                value: Value::Int(SIZEOF_SPA_META_HEADER),
            },
        ],
    };
    let meta_xform_obj = Object {
        type_: pipewire::spa::utils::SpaTypes::ObjectParamMeta.as_raw(),
        id: pipewire::spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            Property {
                key: SPA_PARAM_META_TYPE,
                flags: PropertyFlags::empty(),
                value: Value::Id(pipewire::spa::utils::Id(SPA_META_VIDEO_TRANSFORM as u32)),
            },
            Property {
                key: SPA_PARAM_META_SIZE,
                flags: PropertyFlags::empty(),
                value: Value::Int(SIZEOF_SPA_META_VIDEO_TRANSFORM),
            },
        ],
    };

    let buffers_bytes: Vec<u8> =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(buffers_obj))
            .context("serialize ParamBuffers")?
            .0
            .into_inner();
    let meta_header_bytes: Vec<u8> = PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(meta_header_obj),
    )
    .context("serialize ParamMeta(Header)")?
    .0
    .into_inner();
    let meta_xform_bytes: Vec<u8> = PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(meta_xform_obj),
    )
    .context("serialize ParamMeta(VideoTransform)")?
    .0
    .into_inner();

    let buffers_pod = Pod::from_bytes(&buffers_bytes)
        .ok_or_else(|| anyhow::anyhow!("Pod::from_bytes failed for ParamBuffers"))?;
    let meta_header_pod = Pod::from_bytes(&meta_header_bytes)
        .ok_or_else(|| anyhow::anyhow!("Pod::from_bytes failed for ParamMeta(Header)"))?;
    let meta_xform_pod = Pod::from_bytes(&meta_xform_bytes)
        .ok_or_else(|| anyhow::anyhow!("Pod::from_bytes failed for ParamMeta(VideoTransform)"))?;

    let mut params: [&Pod; 3] = [buffers_pod, meta_header_pod, meta_xform_pod];
    stream
        .update_params(&mut params)
        .map_err(|e| anyhow::anyhow!("update_params: {e}"))?;
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
    // Use libspa's `spa_format_video_raw_parse` (wrapped as VideoInfoRaw) —
    // it walks the pod C-side and knows how to pick the right value out of
    // `Value::Id`, `Value::Rectangle`, AND the Choice variants (Range,
    // Enum, Step, Flags) that Mutter always sends on the first
    // param_changed. The previous hand-rolled parser only handled the
    // fixed-value case, so the first callback returned 0x0/Unknown and the
    // stream never transitioned out of configuring.
    let mut info = pipewire::spa::param::video::VideoInfoRaw::new();
    info.parse(pod).ok()?;
    let size = info.size();
    let fourcc = map_spa_video_format(info.format().as_raw());
    Some(FrameFormat {
        width: size.width,
        height: size.height,
        fourcc,
    })
}

/// Mapping from SPA VideoFormat enum IDs to our simplified pixel layout.
/// Values come from `spa/param/video/raw.h` — stable across SPA versions.
/// Restricted to 4 bytes-per-pixel layouts since `bytes_to_frame` assumes
/// a 4 bpp source; 3 bpp formats like RGB (id 15) must stay Unknown.
fn map_spa_video_format(id: u32) -> PixelFormat {
    match id {
        7 => PixelFormat::Rgbx,  // RGBx
        8 => PixelFormat::Bgrx,  // BGRx (GNOME default)
        11 => PixelFormat::Rgba, // RGBA
        12 => PixelFormat::Bgra, // BGRA
        _ => PixelFormat::Unknown,
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

    // Real dHash so the upstream PerceptualDedup can tell frames apart.
    // Hardcoding 0 here collided every portal frame with the previous one
    // (XOR = 0, below threshold), causing the pipeline to receive only the
    // first frame and then idle on "no new content".
    let hash = crate::capture::compute_hash(&resized);

    Some(ScreenFrame {
        captured_at: Utc::now(),
        image: resized,
        perceptual_hash: hash,
        native_size: Some((fmt.width, fmt.height)),
    })
}
