//! JNI bridge between the Android app (Kotlin) and the shared Rust
//! pipeline in `awareness-core`.
//!
//! Current surface (Phase 1 — screen + notification parity):
//!
//!   init()                 — one-time logging setup.
//!   configure(api_key)     — store the OpenAI key for this process.
//!   analyze(event_json)    — send a ContextEvent to the filter API,
//!                            return the JSON FilterResponse for the
//!                            Kotlin side to turn into a notification.
//!
//! Gating, dedup, budget, and memory are intentionally done on the
//! Kotlin side in Phase 1 (simpler wiring). Later phases can add
//! `gate_evaluate` / `memory_push` JNI calls reusing the same core
//! modules (`awareness_core::gate`, `awareness_core::memory`,
//! `awareness_core::budget`, `awareness_core::dedup`).

use awareness_core::api::OpenAiClient;
use awareness_core::types::{ContextEvent, FilterResponse};
use jni::objects::{JClass, JString};
use jni::sys::jstring;
use jni::JNIEnv;
use std::sync::{Mutex, OnceLock};
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static CLIENT: OnceLock<Mutex<Option<OpenAiClient>>> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("tokio runtime"))
}

fn client_slot() -> &'static Mutex<Option<OpenAiClient>> {
    CLIENT.get_or_init(|| Mutex::new(None))
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_init(
    _env: JNIEnv,
    _class: JClass,
) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("awareness-core"),
    );
    log::info!("awareness-core initialised");
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_configure<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    api_key: JString<'local>,
) {
    let key: String = match env.get_string(&api_key) {
        Ok(s) => s.into(),
        Err(e) => {
            log::error!("configure: bad api_key string: {e}");
            return;
        }
    };
    match OpenAiClient::with_api_key(key) {
        Ok(c) => {
            *client_slot().lock().unwrap() = Some(c);
            log::info!("configure: OpenAiClient ready");
        }
        Err(e) => log::error!("configure: failed to build client: {e}"),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_analyze<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    event_json: JString<'local>,
) -> jstring {
    let raw: String = match env.get_string(&event_json) {
        Ok(s) => s.into(),
        Err(e) => return err_json(&mut env, &format!("bad event string: {e}")),
    };

    let event: ContextEvent = match serde_json::from_str(&raw) {
        Ok(e) => e,
        Err(e) => return err_json(&mut env, &format!("parse ContextEvent: {e}")),
    };

    let client_guard = client_slot().lock().unwrap();
    let Some(client) = client_guard.as_ref() else {
        return err_json(&mut env, "configure() not called");
    };
    let client = client.clone();
    drop(client_guard);

    let response: FilterResponse = runtime().block_on(async move {
        client.filter_call(&event, "").await.unwrap_or_else(|e| {
            FilterResponse {
                should_alert: false,
                alert_type: "error".into(),
                urgency: "low".into(),
                needs_deep_analysis: false,
                quick_message: format!("api error: {e}"),
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                parse_error: Some(e.to_string()),
            }
        })
    });

    let body = serde_json::to_string(&response).unwrap_or_else(|_| "{}".into());
    env.new_string(body)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn err_json(env: &mut JNIEnv, msg: &str) -> jstring {
    let body = serde_json::json!({
        "should_alert": false,
        "alert_type": "error",
        "urgency": "low",
        "needs_deep_analysis": false,
        "quick_message": msg,
        "tokens_in": 0,
        "tokens_out": 0,
        "cost_usd": 0.0,
        "parse_error": msg,
    })
    .to_string();
    env.new_string(body)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
