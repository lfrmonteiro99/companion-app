//! JNI bridge exposing the cross-platform pieces of `awareness-cli`
//! (config, budget, gate, dedup, aggregator, api calls) to the Android app.
//!
//! Everything here must stay free of Linux-specific dependencies
//! (no ashpd, pipewire, cpal, whisper-rs, leptess). Platform capture lives
//! on the Kotlin side (MediaProjection, AudioRecord, ML Kit OCR) and hands
//! text/audio into this core via `submitContext`.

use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Runtime::new().expect("tokio runtime")
    })
}

#[derive(Debug, Deserialize)]
struct ContextInput {
    app: Option<String>,
    window_title: Option<String>,
    screen_text: String,
    mic_text: Option<String>,
    duration_on_app_seconds: u64,
}

#[derive(Debug, Serialize)]
struct CoreResponse {
    should_alert: bool,
    alert_type: String,
    urgency: String,
    message: String,
    tokens_in: u32,
    tokens_out: u32,
    cost_usd: f64,
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_init(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("awareness-core"),
    );
    log::info!("awareness-core initialised");
    1
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_submitContext<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    json_input: JString<'local>,
) -> jstring {
    let input: String = match env.get_string(&json_input) {
        Ok(s) => s.into(),
        Err(e) => return error_response(&mut env, &format!("bad input: {e}")),
    };

    let parsed: ContextInput = match serde_json::from_str(&input) {
        Ok(p) => p,
        Err(e) => return error_response(&mut env, &format!("parse: {e}")),
    };

    // TODO: wire into awareness_cli::{gate, api, budget, dedup}
    // once the shared modules are factored out of the CLI main loop.
    let response = runtime().block_on(async move {
        CoreResponse {
            should_alert: false,
            alert_type: "none".into(),
            urgency: "low".into(),
            message: format!(
                "stub: saw {} chars of screen, app={:?}",
                parsed.screen_text.len(),
                parsed.app
            ),
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
        }
    });

    let body = serde_json::to_string(&response).unwrap_or_else(|_| "{}".into());
    env.new_string(body)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn error_response(env: &mut JNIEnv, msg: &str) -> jstring {
    let body = serde_json::json!({
        "should_alert": false,
        "alert_type": "error",
        "urgency": "low",
        "message": msg,
        "tokens_in": 0,
        "tokens_out": 0,
        "cost_usd": 0.0,
    })
    .to_string();
    env.new_string(body)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
