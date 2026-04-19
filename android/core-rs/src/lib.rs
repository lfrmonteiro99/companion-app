//! JNI bridge between the Android app (Kotlin) and the shared Rust
//! pipeline in `awareness-core`.
//!
//! JNI surface (Phase 1 — screen + notification parity with Linux flow):
//!
//!   init()                         one-time logging setup
//!   configure(api_key, budget)     store key + build Config/state/memory
//!   analyze(event_json) -> json    run gate + memory + API, return decision
//!
//! The tick loop lives on the Kotlin side (MediaProjection requires it).
//! Each tick Kotlin builds a ContextEvent from OCR + mic text + focused
//! app, hands it here, and we:
//!
//!   1. Run `awareness_core::gate::evaluate` with persisted GateState.
//!      If the rule set says Skip, return a no-alert response with the
//!      reason in `alert_type` and no API cost.
//!   2. If Send, call `OpenAiClient::filter_call` with the memory ring's
//!      prompt lines as context.
//!   3. If the model decides `should_alert`, push a MemoryEntry so the
//!      next tick has rolling history (avoids repeat alerts).

use awareness_core::api::OpenAiClient;
use awareness_core::config::Config;
use awareness_core::gate::{self, GateAction, GateDecision, GateState};
use awareness_core::memory::{MemoryEntry, MemoryRing};
use awareness_core::types::{ContextEvent, FilterResponse};
use jni::objects::{JClass, JString};
use jni::sys::{jdouble, jstring};
use jni::JNIEnv;
use std::sync::{Mutex, OnceLock};
use tokio::runtime::Runtime;

const MEMORY_CAPACITY: usize = 10;

struct CoreState {
    client: OpenAiClient,
    config: Config,
    gate: GateState,
    memory: MemoryRing,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<Mutex<Option<CoreState>>> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("tokio runtime"))
}

fn state_slot() -> &'static Mutex<Option<CoreState>> {
    STATE.get_or_init(|| Mutex::new(None))
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
    budget_usd_daily: jdouble,
) {
    let key: String = match env.get_string(&api_key) {
        Ok(s) => s.into(),
        Err(e) => {
            log::error!("configure: bad api_key string: {e}");
            return;
        }
    };
    let config = Config::for_android(key, budget_usd_daily);
    let client = match OpenAiClient::with_api_key(config.openai_api_key.clone()) {
        Ok(c) => c,
        Err(e) => {
            log::error!("configure: failed to build OpenAiClient: {e}");
            return;
        }
    };
    *state_slot().lock().unwrap() = Some(CoreState {
        client,
        config,
        gate: GateState::default(),
        memory: MemoryRing::new(MEMORY_CAPACITY),
    });
    log::info!("configure: core state ready");
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

    let mut guard = state_slot().lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return err_json(&mut env, "configure() not called");
    };

    // 1. Gate.
    let decision: GateDecision = gate::evaluate(&event, &mut state.gate, &state.config);
    if decision.action == GateAction::Skip {
        log::info!("gate skip: {}", decision.reason);
        return ok_json(
            &mut env,
            &FilterResponse {
                should_alert: false,
                alert_type: format!("skipped:{}", decision.reason),
                urgency: "low".into(),
                needs_deep_analysis: false,
                quick_message: String::new(),
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                parse_error: None,
            },
        );
    }

    // 2. API call with rolling memory as context.
    let memory_lines = state.memory.to_prompt_lines();
    let client = state.client.clone();
    // Drop the mutex while the HTTP call is in flight — Kotlin invokes
    // analyze from a background coroutine and may schedule another tick.
    let event_for_api = event.clone();
    drop(guard);

    let response: FilterResponse = runtime().block_on(async move {
        client
            .filter_call(&event_for_api, &memory_lines)
            .await
            .unwrap_or_else(|e| FilterResponse {
                should_alert: false,
                alert_type: "error".into(),
                urgency: "low".into(),
                needs_deep_analysis: false,
                quick_message: format!("api error: {e}"),
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                parse_error: Some(e.to_string()),
            })
    });

    // 3. Push to memory if the model decided to alert.
    if response.should_alert && response.parse_error.is_none() {
        let mut guard = state_slot().lock().unwrap();
        if let Some(state) = guard.as_mut() {
            state.memory.push(MemoryEntry {
                timestamp: event.timestamp,
                app: event.app.clone(),
                alert_type: response.alert_type.clone(),
                should_alert: true,
                quick_message: response.quick_message.clone(),
            });
        }
    }

    ok_json(&mut env, &response)
}

fn ok_json(env: &mut JNIEnv, response: &FilterResponse) -> jstring {
    let body = serde_json::to_string(response).unwrap_or_else(|_| "{}".into());
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
