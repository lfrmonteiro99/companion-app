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
use awareness_core::budget::BudgetController;
use awareness_core::config::Config;
use awareness_core::gate::{self, GateAction, GateDecision, GateState};
use awareness_core::memory::{MemoryEntry, MemoryRing};
use awareness_core::types::{ContextEvent, FilterResponse};
use awareness_core::user_profile::UserProfile;
use jni::objects::{JClass, JString};
use jni::sys::{jdouble, jstring};
use jni::JNIEnv;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tokio::runtime::Runtime;

const MEMORY_CAPACITY: usize = 10;
/// How many past alerted screens we keep fingerprints for — any new
/// tick whose screen_text_excerpt matches one of these by trigram
/// similarity ≥ SCREEN_DUP_THRESHOLD is short-circuited BEFORE the API
/// call, so we don't pay tokens to re-confirm what we already alerted.
const SCREEN_FINGERPRINT_CAPACITY: usize = 16;
const SCREEN_DUP_THRESHOLD: f32 = 0.7;

struct CoreState {
    client: OpenAiClient,
    config: Config,
    gate: GateState,
    memory: MemoryRing,
    budget: BudgetController,
    /// screen_text_excerpts that already produced a user-visible
    /// alert. Walked by jaccard_trigrams on the next tick to drop
    /// duplicates before spending API tokens.
    alerted_screens: std::collections::VecDeque<String>,
    /// Persistent user profile (bio + interests + anti-interests +
    /// app usage) — prepended to the system prompt so the model
    /// tailors alerts instead of treating the user as generic.
    profile: UserProfile,
    profile_path: PathBuf,
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
pub extern "system" fn Java_com_companion_awareness_CoreBridge_init(_env: JNIEnv, _class: JClass) {
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
    files_dir: JString<'local>,
) {
    let key: String = match env.get_string(&api_key) {
        Ok(s) => s.into(),
        Err(e) => {
            log::error!("configure: bad api_key string: {e}");
            return;
        }
    };
    let dir: String = match env.get_string(&files_dir) {
        Ok(s) => s.into(),
        Err(e) => {
            log::error!("configure: bad files_dir string: {e}");
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
    let files_dir = PathBuf::from(&dir);
    let budget = BudgetController::new(budget_usd_daily, &files_dir);
    let profile_path = files_dir.join("user_profile.json");
    let profile = UserProfile::load(&profile_path);
    *state_slot().lock().unwrap() = Some(CoreState {
        client,
        config,
        gate: GateState::default(),
        memory: MemoryRing::new(MEMORY_CAPACITY),
        budget,
        alerted_screens: std::collections::VecDeque::with_capacity(SCREEN_FINGERPRINT_CAPACITY),
        profile,
        profile_path,
    });
    log::info!("configure: core state ready (budget ${budget_usd_daily:.2}/day)");
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
            &FilterResponse::short_circuit(format!("skipped:{}", decision.reason), ""),
        );
    }

    // 2. Budget precheck. A single filter_call on gpt-4o-mini is roughly
    // $0.0005–$0.002 — if we have less than 1/10th of a cent left we
    // consider the day over.
    state.budget.reset_if_new_day();
    if state.budget.remaining() < 0.001 {
        log::warn!(
            "budget exceeded (spent ${:.4}/${:.4}); skipping API call",
            state.budget.spent(),
            state.config.budget_usd_daily,
        );
        return ok_json(
            &mut env,
            &FilterResponse::short_circuit(
                "budget_exceeded",
                format!(
                    "Daily budget of ${:.2} exhausted. Alerts paused until tomorrow.",
                    state.config.budget_usd_daily,
                ),
            ),
        );
    }

    // 2b. Pre-API dedup: if this screen text was already the basis
    //     of a recent alert (jaccard ≥ 0.7), skip the model call
    //     entirely. Saves the input tokens for situations where the
    //     page didn't meaningfully change since the last alert (e.g.
    //     the user is still on the same diff view / email / post).
    let current_screen = event.screen_text_excerpt.clone();
    let already_alerted = state.alerted_screens.iter().any(|past| {
        awareness_core::dedup::TextDedup::jaccard_trigrams(past, &current_screen)
            >= SCREEN_DUP_THRESHOLD
    });
    if already_alerted {
        log::info!(
            "pre-api dedup: skipping model call, screen already alerted (chars={})",
            current_screen.len(),
        );
        return ok_json(
            &mut env,
            &FilterResponse::short_circuit("skipped:already_alerted", ""),
        );
    }

    // Track app usage passively every tick; feeds "top apps"
    // heuristic in the user profile.
    if let Some(app) = event.app.as_deref() {
        state.profile.record_app_usage(app);
    }

    // 3. API call with rolling memory as context.
    let memory_lines = state.memory.to_prompt_lines();
    let profile_ctx = state.profile.to_prompt_context();
    // Pick the subset of the user's explicit interests that actually
    // match the current screen. Empty when none match — in that case
    // the prompt omits the line entirely.
    let matched_interests = state.profile.filter_interests_for_screen(
        &event.screen_text_excerpt,
        event.window_title.as_deref(),
        event.app.as_deref(),
    );
    let client = state.client.clone();
    // Drop the mutex while the HTTP call is in flight — Kotlin invokes
    // analyze from a background coroutine and may schedule another tick.
    let event_for_api = event.clone();
    drop(guard);

    let matched_for_api = matched_interests.clone();
    let response: FilterResponse = runtime().block_on(async move {
        client
            .filter_call(&event_for_api, &memory_lines, &profile_ctx, &matched_for_api)
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
                matched_interests: matched_for_api.clone(),
            })
    });

    // 4. Deduct actual cost + apply anti-repetition + push to memory.
    let mut response = response;
    {
        let mut guard = state_slot().lock().unwrap();
        if let Some(state) = guard.as_mut() {
            if let Err(over) = state.budget.try_spend(response.cost_usd) {
                log::warn!(
                    "budget tipped over while deducting call cost: spent ${:.4} of ${:.4}",
                    over.spent,
                    over.limit,
                );
            }

            // Client-side anti-repetition. The prompt asks the model to
            // stay silent when the history already covers the same
            // situation, but gpt-4.1-mini sometimes re-alerts with
            // near-identical text anyway. Compare the proposed
            // quick_message against every memory entry via the same
            // Jaccard trigram similarity the desktop pipeline uses for
            // OCR dedup. ≥0.7 means "same alert, new timestamp" and we
            // override to should_alert=false so nothing reaches the
            // user.
            if response.should_alert && response.parse_error.is_none() {
                let dup = state.memory.entries().any(|e| {
                    awareness_core::dedup::TextDedup::jaccard_trigrams(
                        &response.quick_message,
                        &e.quick_message,
                    ) >= 0.7
                });
                if dup {
                    log::info!(
                        "anti-repeat: suppressing duplicate alert ({:?})",
                        response.quick_message,
                    );
                    response.should_alert = false;
                    response.alert_type = "duplicate".into();
                } else {
                    state.memory.push(MemoryEntry {
                        timestamp: event.timestamp,
                        app: event.app.clone(),
                        alert_type: response.alert_type.clone(),
                        should_alert: true,
                        quick_message: response.quick_message.clone(),
                    });
                    // Remember the screen fingerprint so the next tick
                    // short-circuits before the API call.
                    if state.alerted_screens.len() == SCREEN_FINGERPRINT_CAPACITY {
                        state.alerted_screens.pop_front();
                    }
                    state.alerted_screens.push_back(current_screen.clone());
                }
            }
            // Persist the updated profile (app usage counter bumped
            // earlier in this handler). Cheap JSON write; skipped on
            // failure so analyze never blows up over a filesystem
            // hiccup.
            let _ = state.profile.save(&state.profile_path);
        }
    }

    ok_json(&mut env, &response)
}

// ── User profile JNI surface ────────────────────────────────────────

fn with_state_mut<F: FnOnce(&mut CoreState)>(f: F) {
    if let Some(state) = state_slot().lock().unwrap().as_mut() {
        f(state);
    }
}

fn state_profile_save(state: &CoreState) {
    let _ = state.profile.save(&state.profile_path);
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_setBio<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    bio: JString<'local>,
) {
    let bio: String = match env.get_string(&bio) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    with_state_mut(|state| {
        state.profile.set_bio(bio);
        state_profile_save(state);
    });
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_learnInterest<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    topic: JString<'local>,
    positive: jni::sys::jboolean,
) {
    let topic: String = match env.get_string(&topic) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    with_state_mut(|state| {
        if positive != 0 {
            state.profile.add_interest(&topic);
        } else {
            state.profile.add_anti_interest(&topic);
        }
        state_profile_save(state);
    });
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_setExplicitInterests<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    items: jni::objects::JObjectArray<'local>,
) {
    let len = match env.get_array_length(&items) {
        Ok(l) => l,
        Err(_) => return,
    };
    let mut out: Vec<String> = Vec::with_capacity(len.max(0) as usize);
    for i in 0..len {
        let Ok(obj) = env.get_object_array_element(&items, i) else {
            continue;
        };
        let s: jni::objects::JString = obj.into();
        let Ok(js) = env.get_string(&s) else { continue };
        let value: String = js.into();
        out.push(value);
    }
    with_state_mut(|state| {
        state.profile.set_explicit_interests(out);
        state_profile_save(state);
    });
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_getExplicitInterests<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    let body = state_slot()
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| {
            serde_json::to_string(s.profile.list_explicit_interests())
                .unwrap_or_else(|_| "[]".into())
        })
        .unwrap_or_else(|| "[]".into());
    env.new_string(body)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_com_companion_awareness_CoreBridge_getProfileText<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    let body = state_slot()
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| {
            serde_json::json!({
                "bio": s.profile.bio,
                "interests": s.profile.interests,
                "anti_interests": s.profile.anti_interests,
                "top_apps": s.profile.to_prompt_context(),
            })
            .to_string()
        })
        .unwrap_or_else(|| "{}".to_string());
    env.new_string(body)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
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
